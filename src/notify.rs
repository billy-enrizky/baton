use std::io::Write;
use std::time::{Duration, Instant};

pub const BELL: u8 = 0x07;
pub const MIN_INTERVAL: Duration = Duration::from_secs(2);

pub fn emit_bell(out: &mut impl Write) -> std::io::Result<()> {
    out.write_all(&[BELL])?;
    out.flush()
}

/// One bell per interval, and only when the user asked for it.
#[derive(Debug, Default)]
pub struct BellLimiter {
    last: Option<Instant>,
}

impl BellLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn fire(&mut self, enabled: bool, now: Instant) -> bool {
        if !enabled {
            return false;
        }
        if let Some(last) = self.last {
            if now.saturating_duration_since(last) < MIN_INTERVAL {
                return false;
            }
        }
        self.last = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_never_fires() {
        let mut limiter = BellLimiter::new();
        assert!(!limiter.fire(false, Instant::now()));
    }

    #[test]
    fn enabled_fires_once_then_mutes_within_the_interval() {
        let mut limiter = BellLimiter::new();
        let start = Instant::now();
        assert!(limiter.fire(true, start));
        assert!(!limiter.fire(true, start + Duration::from_millis(1999)));
        assert!(limiter.fire(true, start + MIN_INTERVAL));
    }

    #[test]
    fn emit_writes_exactly_one_bell_byte() {
        let mut sink = Vec::new();
        emit_bell(&mut sink).unwrap();
        assert_eq!(sink, vec![BELL]);
    }
}
