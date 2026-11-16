//! V12.6 — request-quota budget ledger.
//!
//! Hyperliquid has **no** endpoint that reports remaining REST head-room: the
//! only place the numbers appear is the rejection string
//!
//! ```text
//! Too many cumulative requests sent (25307 > 21140)
//! for cumulative volume traded $11141.57.
//! Place taker orders to free up 1 request per USDC traded.
//! ```
//!
//! `(sent, allowed)` there is the venue's own running total, so it is a
//! perfect anchor. Every request *we* issue after that anchor moves the
//! estimate forward locally, which means we can predict the next rejection
//! **before** spending the request.
//!
//! The estimate is deliberately one-sided: fills replenish the allowance and
//! we cannot see them, so `used_est` can only be too high → `headroom_est`
//! can only be too low. Safe to stop on, never safe to squeeze the last
//! request out of.

use std::time::{Duration, Instant};

/// How aggressively to back off once the budget is exhausted.
const COOLDOWN: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Enough head-room for the requested action.
    Allow,
    /// Budget exhausted; wait, and retry afterwards.
    Wait { after: Duration },
    /// Budget exhausted *and* nothing has moved for a while — only volume
    /// traded at the venue can unlock more.
    MustTrade { need: u64 },
}

#[derive(Debug, Clone)]
pub struct QuotaGauge {
    pub used_est: Option<u64>,
    pub limit: Option<u64>,
    pub headroom_est: Option<i64>,
    pub requests_per_usdc: Option<f64>,
    pub usdc_to_unlock: Option<f64>,
    pub sent_since_sync: u64,
    pub deferred: u64,
    pub state: &'static str,
}

impl QuotaGauge {
    /// One line, ready for a log scraper or a Prometheus textfile.
    pub fn line(&self) -> String {
        format!(
            "QUOTA state={} headroom={} used={} limit={} ratio={} need_usdc={} since_sync={} deferred={}",
            self.state,
            fmt_opt_i64(self.headroom_est),
            fmt_opt_u64(self.used_est),
            fmt_opt_u64(self.limit),
            self.requests_per_usdc
                .map(|r| format!("{:.2}", r))
                .unwrap_or_else(|| "-".into()),
            self.usdc_to_unlock
                .map(|u| format!("{:.0}", u))
                .unwrap_or_else(|| "-".into()),
            self.sent_since_sync,
            self.deferred,
        )
    }
}

fn fmt_opt_u64(v: Option<u64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

fn fmt_opt_i64(v: Option<i64>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "-".into())
}

#[derive(Debug, Default)]
pub struct QuotaLedger {
    limit: Option<u64>,
    used_at_sync: Option<u64>,
    volume_at_sync: Option<f64>,
    sent_since_sync: u64,
    deferred: u64,
    last_sync: Option<Instant>,
    last_reject: Option<Instant>,
}

/// Parse `... requests sent (A > B) ...` and an optional `$V`.
pub fn parse_quota_text(text: &str) -> Option<(u64, u64, Option<f64>)> {
    let anchor = "requests sent (";
    let start = text.find(anchor)?.checked_add(anchor.len())?;
    let rest = text.get(start..)?;
    let end = rest.find(')')?;
    let pair = rest.get(..end)?;
    let mut parts = pair.split('>');
    let sent = parts.next()?.trim().parse::<u64>().ok()?;
    let allowed = parts.next()?.trim().parse::<u64>().ok()?;

    let volume = text
        .find("volume traded $")
        .and_then(|i| text.get(i + "volume traded $".len()..))
        .map(|s| s.trim_start())
        .and_then(|s| {
            let take: String = s
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == ',')
                .collect();
            // The sentence ends with a period: "… traded $11141.57."
            take.trim_end_matches('.')
                .replace(',', "")
                .parse::<f64>()
                .ok()
        });

    Some((sent, allowed, volume))
}

impl QuotaLedger {
    /// Seed from a known-good observation (used by the bench, and by tests).
    pub fn seed(&mut self, sent: u64, allowed: u64, volume: Option<f64>) {
        self.limit = Some(allowed);
        self.used_at_sync = Some(sent);
        self.volume_at_sync = volume;
        self.sent_since_sync = 0;
        self.last_sync = Some(Instant::now());
    }

    /// One request is about to be issued.
    pub fn on_request(&mut self) {
        self.sent_since_sync = self.sent_since_sync.saturating_add(1);
    }

    /// The venue rejected a request for quota reasons.
    pub fn on_reject(&mut self, msg: &str) -> bool {
        self.last_reject = Some(Instant::now());
        match parse_quota_text(msg) {
            Some((sent, allowed, volume)) => {
                self.limit = Some(allowed);
                self.used_at_sync = Some(sent);
                if volume.is_some() {
                    self.volume_at_sync = volume;
                }
                self.sent_since_sync = 0;
                self.last_sync = Some(Instant::now());
                true
            }
            None => false,
        }
    }

    pub fn used_est(&self) -> Option<u64> {
        self.used_at_sync
            .map(|used| used.saturating_add(self.sent_since_sync))
    }

    pub fn headroom_est(&self) -> Option<i64> {
        match (self.limit, self.used_est()) {
            (Some(limit), Some(used)) => Some(limit as i64 - used as i64),
            _ => None,
        }
    }

    /// Requests granted per USDC of cumulative volume traded (≈1.9 on the
    /// observed account). Drifts slowly as volume grows.
    pub fn requests_per_usdc(&self) -> Option<f64> {
        match (self.limit, self.volume_at_sync) {
            (Some(limit), Some(volume)) if volume > 0.0 => Some(limit as f64 / volume),
            _ => None,
        }
    }

    /// How much extra traded volume would unlock `need` requests.
    pub fn usdc_to_unlock(&self, need: u64) -> Option<f64> {
        let ratio = self.requests_per_usdc()?;
        if ratio <= 0.0 {
            return None;
        }
        Some(need as f64 / ratio)
    }

    /// Decide whether `need` more requests can be afforded. `reserve` keeps a
    /// safety margin — the estimate is pessimistic, but boundary cases are
    /// where it is least trustworthy.
    pub fn gate(&self, need: u64, reserve: u64) -> Gate {
        let Some(headroom) = self.headroom_est() else {
            return Gate::Allow; // no anchor yet: learn by trying
        };
        if headroom >= need as i64 + reserve as i64 {
            return Gate::Allow;
        }
        // Budget exhausted: if nothing has replenished for a while, only
        // traded volume can help.
        let idle = self
            .last_reject
            .map(|t| t.elapsed())
            .unwrap_or(Duration::ZERO);
        if idle > COOLDOWN && headroom < 0 {
            let deficit = (-headroom).max(1) as u64 + need + reserve;
            Gate::MustTrade {
                need: deficit,
            }
        } else {
            let wait = COOLDOWN.saturating_sub(idle);
            Gate::Wait {
                after: if wait.is_zero() {
                    Duration::from_secs(1)
                } else {
                    wait
                },
            }
        }
    }

    pub fn note_deferred(&mut self) {
        self.deferred = self.deferred.saturating_add(1);
    }

    pub fn gauge(&self, reserve: u64) -> QuotaGauge {
        let headroom = self.headroom_est();
        let state = match headroom {
            None => "unknown",
            Some(h) if h >= reserve as i64 => "ok",
            Some(h) if h >= -(reserve as i64) => "tight",
            Some(_) => "conserve",
        };
        let need = headroom.map(|h| (-h).max(0) as u64 + reserve).unwrap_or(0);
        QuotaGauge {
            used_est: self.used_est(),
            limit: self.limit,
            headroom_est: headroom,
            requests_per_usdc: self.requests_per_usdc(),
            usdc_to_unlock: if need > 0 {
                self.usdc_to_unlock(need)
            } else {
                Some(0.0)
            },
            sent_since_sync: self.sent_since_sync,
            deferred: self.deferred,
            state,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = "Too many cumulative requests sent (25307 > 21140) for cumulative volume traded $11141.57. Place taker orders to free up 1 request per USDC traded.";
    const REAL2: &str = "Too many cumulative requests sent (25241 > 21140) for cumulative volume traded $11141.57. Place taker orders to free up 1 request per USDC traded.";

    #[test]
    fn parses_real_rejection() {
        let (sent, allowed, volume) = parse_quota_text(REAL).expect("parse");
        assert_eq!((sent, allowed), (25307, 21140));
        assert!((volume.unwrap() - 11141.57).abs() < 1e-6);
    }

    #[test]
    fn parses_without_volume() {
        let (sent, allowed, volume) = parse_quota_text("requests sent (10 > 5)").expect("parse");
        assert_eq!((sent, allowed), (10, 5));
        assert!(volume.is_none());
    }

    #[test]
    fn garbage_does_not_parse() {
        assert!(parse_quota_text("something else entirely").is_none());
        assert!(parse_quota_text("requests sent (abc > def)").is_none());
    }

    #[test]
    fn headroom_moves_with_local_requests() {
        let mut l = QuotaLedger::default();
        l.on_reject(REAL2);
        assert_eq!(l.headroom_est(), Some(-4101));
        for _ in 0..3 {
            l.on_request();
        }
        assert_eq!(l.headroom_est(), Some(-4104));
        // A fresh rejection re-anchors the estimate.
        l.on_reject(REAL);
        assert_eq!(l.headroom_est(), Some(-4167));
        assert_eq!(l.sent_since_sync, 0);
    }

    #[test]
    fn ratio_and_unlock_cost() {
        let mut l = QuotaLedger::default();
        l.on_reject(REAL);
        let ratio = l.requests_per_usdc().unwrap();
        assert!((ratio - 1.897).abs() < 0.01, "ratio={ratio}");
        // -4167 + 2 reserve ≈ 4169 requests → ≈ 2197 USDC traded.
        let usdc = l.usdc_to_unlock(4169).unwrap();
        assert!((2190.0..2210.0).contains(&usdc), "usdc={usdc}");
    }

    #[test]
    fn gate_allows_when_headroom_is_healthy() {
        let mut l = QuotaLedger::default();
        l.seed(1000, 21140, Some(11141.57));
        assert_eq!(l.gate(5, 3), Gate::Allow);
    }

    #[test]
    fn gate_blocks_when_over_budget() {
        let mut l = QuotaLedger::default();
        l.on_reject(REAL); // headroom -4167
        match l.gate(1, 2) {
            Gate::Wait { .. } | Gate::MustTrade { .. } => {}
            Gate::Allow => panic!("must not allow"),
        }
    }

    #[test]
    fn seed_overrides_previous_anchor() {
        let mut l = QuotaLedger::default();
        l.on_reject(REAL);
        l.seed(21000, 21140, Some(12000.0));
        assert_eq!(l.headroom_est(), Some(140));
        assert_eq!(l.gauge(3).state, "ok");
    }

    #[test]
    fn gauge_states() {
        let mut l = QuotaLedger::default();
        assert_eq!(l.gauge(3).state, "unknown");
        l.seed(21000, 21140, Some(11200.0));
        assert_eq!(l.gauge(3).state, "ok");
        l.seed(21139, 21140, Some(11200.0));
        assert_eq!(l.gauge(3).state, "tight");
        l.seed(22000, 21140, Some(11200.0));
        assert_eq!(l.gauge(3).state, "conserve");
        assert!(l.gauge(3).line().contains("state=conserve"));
    }
}
