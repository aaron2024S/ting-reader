use super::*;

impl JsBudget {
    pub fn release_probe(&self) -> impl Fn() -> bool + use<> {
        let weak = Arc::downgrade(&self.0);
        move || weak.upgrade().is_none()
    }

    pub(super) fn reference_count(&self) -> usize {
        Arc::strong_count(&self.0)
    }

    pub fn external_bytes(&self) -> usize {
        self.0.external_bytes.load(Ordering::SeqCst)
    }
}

#[test]
fn backing_store_growth_and_release_balance_the_budget() {
    let budget = JsBudget::new(32 * 1024 * 1024).unwrap();
    unsafe {
        let pointer = allocate(&budget.0, 1024);
        assert!(!pointer.is_null());
        let pointer = reallocate(&budget.0, pointer, 1024, 2048);
        assert!(!pointer.is_null());
        assert_eq!(budget.0.external_bytes.load(Ordering::SeqCst), 2048);
        let pointer = reallocate(&budget.0, pointer, 2048, 512);
        assert!(!pointer.is_null());
        free(&budget.0, pointer, 512);
    }
    assert_eq!(budget.0.external_bytes.load(Ordering::SeqCst), 0);
}

#[test]
fn disarming_and_dropping_monitors_release_their_state() {
    let budget = JsBudget::new(32 * 1024 * 1024).unwrap();
    let released = budget.release_probe();
    for _ in 0..20 {
        let monitor = ExecutionMonitor::new(budget.clone()).unwrap();
        let lease = monitor.begin(Duration::from_secs(10)).unwrap();
        let token = lease.cancellation.clone();
        lease.finish();
        assert!(token.is_cancelled());
        assert!(monitor.clock.state.lock().unwrap().deadline.is_none());
        drop(monitor);
        assert_eq!(budget.reference_count(), 1);
    }
    drop(budget);
    assert!(released());
}
