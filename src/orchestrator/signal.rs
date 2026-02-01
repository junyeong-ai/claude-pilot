use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    None,
    Pause,
    Cancel,
}

impl From<u8> for Signal {
    fn from(v: u8) -> Self {
        match v {
            1 => Self::Pause,
            2 => Self::Cancel,
            _ => Self::None,
        }
    }
}

impl From<Signal> for u8 {
    fn from(s: Signal) -> Self {
        match s {
            Signal::None => 0,
            Signal::Pause => 1,
            Signal::Cancel => 2,
        }
    }
}

#[derive(Clone)]
pub struct SignalHandler {
    signal: Arc<AtomicU8>,
    acknowledged: Arc<AtomicBool>,
}

impl Default for SignalHandler {
    fn default() -> Self {
        Self::new()
    }
}

impl SignalHandler {
    pub fn new() -> Self {
        Self {
            signal: Arc::new(AtomicU8::new(0)),
            acknowledged: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn send(&self, signal: Signal) {
        self.acknowledged.store(false, Ordering::SeqCst);
        self.signal.store(signal.into(), Ordering::SeqCst);
    }

    pub fn pause(&self) {
        self.send(Signal::Pause);
    }

    pub fn cancel(&self) {
        self.send(Signal::Cancel);
    }

    pub fn clear(&self) {
        self.signal.store(0, Ordering::SeqCst);
        self.acknowledged.store(false, Ordering::SeqCst);
    }

    pub fn check(&self) -> Signal {
        Signal::from(self.signal.load(Ordering::SeqCst))
    }

    pub fn has_signal(&self) -> bool {
        self.check() != Signal::None
    }

    pub fn acknowledge(&self) {
        self.acknowledged.store(true, Ordering::SeqCst);
    }

    pub fn is_acknowledged(&self) -> bool {
        self.acknowledged.load(Ordering::SeqCst)
    }

    pub fn check_and_acknowledge(&self) -> Signal {
        let signal = self.check();
        if signal != Signal::None {
            self.acknowledge();
        }
        signal
    }
}
