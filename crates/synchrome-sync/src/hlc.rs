//! Hybrid logical clock (`docs/03` §2).
//!
//! Serialised as `"<wallMs:13>-<counter:4>-<deviceId>"` so that plain string order equals
//! clock order (ties broken by device id). A device refuses to generate timestamps when its
//! wall clock is more than 24 h behind the highest HLC it has observed.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Maximum tolerated backwards wall-clock distance before the device refuses to emit ops.
pub const MAX_BACKWARD_SKEW_MS: u64 = 24 * 60 * 60 * 1000;
/// Skew above which the UI should warn.
pub const WARN_SKEW_MS: u64 = 5 * 60 * 1000;
const MAX_COUNTER: u32 = 9999;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Hlc {
    pub wall_ms: u64,
    pub counter: u32,
    pub device_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HlcError {
    #[error("invalid HLC string")]
    Parse,
    #[error("wall clock is {behind_ms} ms behind the highest observed HLC; refusing to emit ops")]
    ClockBehind { behind_ms: u64 },
    #[error("HLC counter overflow")]
    CounterOverflow,
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:013}-{:04}-{}",
            self.wall_ms, self.counter, self.device_id
        )
    }
}

impl std::str::FromStr for Hlc {
    type Err = HlcError;
    fn from_str(s: &str) -> Result<Self, HlcError> {
        let (wall, rest) = s.split_once('-').ok_or(HlcError::Parse)?;
        let (counter, device) = rest.split_once('-').ok_or(HlcError::Parse)?;
        if wall.len() != 13 || counter.len() != 4 || device.is_empty() {
            return Err(HlcError::Parse);
        }
        Ok(Hlc {
            wall_ms: wall.parse().map_err(|_| HlcError::Parse)?,
            counter: counter.parse().map_err(|_| HlcError::Parse)?,
            device_id: device.to_string(),
        })
    }
}

/// The local clock: remembers the highest HLC issued or observed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HlcClock {
    pub device_id: String,
    pub last: Option<Hlc>,
}

impl HlcClock {
    pub fn new(device_id: impl Into<String>, last: Option<Hlc>) -> Self {
        Self {
            device_id: device_id.into(),
            last,
        }
    }

    /// Issue the next timestamp given the current wall clock.
    pub fn tick(&mut self, now_ms: u64) -> Result<Hlc, HlcError> {
        let next = match &self.last {
            None => Hlc {
                wall_ms: now_ms,
                counter: 0,
                device_id: self.device_id.clone(),
            },
            Some(last) => {
                if last.wall_ms > now_ms && last.wall_ms - now_ms > MAX_BACKWARD_SKEW_MS {
                    return Err(HlcError::ClockBehind {
                        behind_ms: last.wall_ms - now_ms,
                    });
                }
                if now_ms > last.wall_ms {
                    Hlc {
                        wall_ms: now_ms,
                        counter: 0,
                        device_id: self.device_id.clone(),
                    }
                } else if last.counter < MAX_COUNTER {
                    Hlc {
                        wall_ms: last.wall_ms,
                        counter: last.counter + 1,
                        device_id: self.device_id.clone(),
                    }
                } else {
                    // Counter exhausted within one millisecond: borrow the next millisecond.
                    Hlc {
                        wall_ms: last.wall_ms + 1,
                        counter: 0,
                        device_id: self.device_id.clone(),
                    }
                }
            }
        };
        self.last = Some(next.clone());
        Ok(next)
    }

    /// Merge a remote timestamp (so later local ops sort after everything seen).
    pub fn observe(&mut self, remote: &Hlc) {
        if self.last.as_ref().map_or(true, |l| remote > l) {
            self.last = Some(Hlc {
                device_id: self.device_id.clone(),
                ..remote.clone()
            });
        }
    }
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_order_is_clock_order_and_round_trips() {
        let mut c = HlcClock::new("dev-a", None);
        let a = c.tick(1_790_000_000_000).unwrap();
        let b = c.tick(1_790_000_000_000).unwrap();
        let d = c.tick(1_789_999_999_000).unwrap(); // wall went back slightly: still monotonic
        let e = c.tick(1_790_000_000_005).unwrap();
        assert_eq!(a.to_string(), "1790000000000-0000-dev-a");
        assert!(a < b && b < d && d < e);
        assert!(a.to_string() < b.to_string() && d.to_string() < e.to_string());
        assert_eq!(b.to_string().parse::<Hlc>().unwrap(), b);
        assert!("junk".parse::<Hlc>().is_err());
    }

    #[test]
    fn refuses_when_clock_is_a_day_behind() {
        let mut c = HlcClock::new("d", None);
        c.tick(10 * MAX_BACKWARD_SKEW_MS).unwrap();
        assert!(matches!(
            c.tick(8 * MAX_BACKWARD_SKEW_MS),
            Err(HlcError::ClockBehind { .. })
        ));
    }

    #[test]
    fn observe_moves_the_clock_forward() {
        let mut c = HlcClock::new("a", None);
        c.tick(100).unwrap();
        c.observe(&Hlc {
            wall_ms: 500,
            counter: 7,
            device_id: "b".into(),
        });
        let t = c.tick(200).unwrap();
        assert_eq!((t.wall_ms, t.counter, t.device_id.as_str()), (500, 8, "a"));
    }
}
