use std::ops::{Deref, DerefMut};
use std::thread::ThreadId;

/// A thread-safe cell that can only be accessed from the thread it was created on.
/// It will panic if accessed or dropped from a different thread.
#[derive(Debug)]
pub struct SendCell<T> {
    data: T,
    tid: ThreadId,
}

impl<T> SendCell<T> {
    pub fn new(val: T) -> Self {
        SendCell {
            data: val,
            tid: std::thread::current().id(),
        }
    }
}

impl<T> Deref for SendCell<T> {
    type Target = T;

    #[track_caller]
    fn deref(&self) -> &Self::Target {
        if self.tid != std::thread::current().id() {
            panic!("SendCell::deref() called on different thread than it was created on");
        }

        &self.data
    }
}

impl<T> DerefMut for SendCell<T> {
    #[track_caller]
    fn deref_mut(&mut self) -> &mut Self::Target {
        if self.tid != std::thread::current().id() {
            panic!("SendCell::deref_mut() called on different thread than it was created on");
        }

        &mut self.data
    }
}

impl<T> Drop for SendCell<T> {
    #[track_caller]
    fn drop(&mut self) {
        if self.tid != std::thread::current().id() {
            panic!("SendCell dropped on different thread than it was created on");
        }
    }
}

unsafe impl<T> Send for SendCell<T> {}
