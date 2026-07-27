pub struct AbortGuard(futures::future::AbortHandle);

impl AbortGuard {
    pub fn new_pair() -> (Self, futures::future::AbortRegistration) {
        let (abort_handle, abort_registration) = futures::future::AbortHandle::new_pair();
        (Self(abort_handle), abort_registration)
    }
}

impl Drop for AbortGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
