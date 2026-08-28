use std::fmt;
use std::ops::Deref;
use std::sync::Arc;
use std::time::Duration;

use super::{PoolState, ReaderSlot};

pub(crate) struct PooledDigitRead<'source> {
    state: Arc<PoolState<'source>>,
    slot: Option<ReaderSlot<'source>>,
    read: Duration,
    parse: Duration,
    cache_hit: Duration,
}

impl<'source> PooledDigitRead<'source> {
    pub(super) fn new(
        state: Arc<PoolState<'source>>,
        slot: ReaderSlot<'source>,
        read: Duration,
        parse: Duration,
        cache_hit: Duration,
    ) -> Self {
        Self {
            state,
            slot: Some(slot),
            read,
            parse,
            cache_hit,
        }
    }

    pub(crate) const fn read_time(&self) -> Duration {
        self.read
    }

    pub(crate) const fn parse_time(&self) -> Duration {
        self.parse
    }

    pub(crate) const fn cache_hit_time(&self) -> Duration {
        self.cache_hit
    }
}

impl Deref for PooledDigitRead<'_> {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        self.slot.as_ref().map_or(&[], |slot| slot.reader.digits())
    }
}

impl fmt::Debug for PooledDigitRead<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("PooledDigitRead")
            .field(&&**self)
            .finish()
    }
}

impl Drop for PooledDigitRead<'_> {
    fn drop(&mut self) {
        if let Some(slot) = self.slot.take() {
            self.state.return_slot(slot);
        }
    }
}
