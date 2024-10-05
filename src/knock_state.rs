use quanta::Instant;
use std::time::Duration;

const PORT_SEQUENCE: &[u16] = &[123, 436, 1928, 29545];
const PENDING_DURATION: Duration = Duration::from_secs(5);
const PASSED_DURATION: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialOrd, PartialEq)]
pub enum KnockState {
    PortPending { last_idx: usize, expires: Instant },
    Passed { expires: Instant },
    Failed,
}

impl KnockState {
    pub fn try_new(port: u16) -> Self {
        if port == PORT_SEQUENCE[0] {
            Self::PortPending {
                last_idx: 0,
                expires: Instant::recent() + PENDING_DURATION,
            }
        } else {
            Self::Failed
        }
    }

    pub fn progress(self, port: u16) -> Self {
        // Any state other than pending is invalid
        let Self::PortPending { last_idx, .. } = self else {
            return self;
        };

        // Explicit restart
        if port == PORT_SEQUENCE[0] {
            return Self::PortPending {
                last_idx: 0,
                expires: Instant::recent() + PENDING_DURATION,
            };
        }

        // Repeated knock
        if port == PORT_SEQUENCE[last_idx] {
            return Self::PortPending {
                last_idx,
                expires: Instant::recent() + PENDING_DURATION,
            };
        }

        // Bad knock
        if port != PORT_SEQUENCE[last_idx + 1] {
            return Self::Failed;
        }

        // End of sequence
        if last_idx + 2 == PORT_SEQUENCE.len() {
            return Self::Passed {
                expires: Instant::recent() + PASSED_DURATION,
            };
        }

        // Next knock
        Self::PortPending {
            last_idx: last_idx + 1,
            expires: Instant::recent() + PENDING_DURATION,
        }
    }

    pub fn is_valid(&self) -> bool {
        match self {
            KnockState::PortPending {
                expires: expires_at,
                ..
            } => expires_at > &Instant::recent(),
            KnockState::Passed {
                expires: expires_at,
                ..
            } => expires_at > &Instant::recent(),
            // If rust had #[hot] and it worked on match arms, it would go here
            KnockState::Failed => false,
        }
    }

    pub fn expiration(&self) -> Instant {
        match self {
            KnockState::PortPending { expires, .. } => *expires,
            KnockState::Passed { expires } => *expires,
            KnockState::Failed => unreachable!("This should never be called in this cases"),
        }
    }
}
