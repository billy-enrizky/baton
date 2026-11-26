use baton::claim::{acquire, ClaimOutcome, ProcessProbe};
use baton::store::BatonStore;

struct FixedProbe {
    pid: u32,
    start: String,
}

impl ProcessProbe for FixedProbe {
    fn start(&self, pid: u32) -> Option<String> {
        (pid == self.pid).then(|| self.start.clone())
    }

    fn alive(&self, pid: u32, expected_start: Option<&str>) -> bool {
        pid == self.pid && expected_start == Some(self.start.as_str())
    }
}

#[test]
fn a_file_store_claim_is_visible_and_releasable() {
    let dir = tempfile::tempdir().unwrap();
    let store = BatonStore::open(&dir.path().join("baton.sqlite")).unwrap();
    let host_id = uuid::Uuid::new_v4().to_string();
    let probe = FixedProbe {
        pid: std::process::id(),
        start: "integration-start".into(),
    };
    let claim = match acquire(&store, dir.path(), &host_id, &probe).unwrap() {
        ClaimOutcome::Claimed(claim) => claim,
        other => panic!("expected claim, got {other:?}"),
    };
    assert_eq!(claim.host_id, host_id);
    assert_eq!(store.get_host_claim().unwrap().as_ref(), Some(&claim));
    assert!(store.release_host_claim(&host_id).unwrap());
    assert!(store.get_host_claim().unwrap().is_none());
}
