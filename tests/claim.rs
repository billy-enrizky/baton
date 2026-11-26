use baton::claim::{acquire, ClaimOutcome, SystemProbe};
use baton::store::BatonStore;

#[cfg(unix)]
#[test]
fn a_file_store_claim_is_visible_and_releasable() {
    let dir = tempfile::tempdir().unwrap();
    let host_id = uuid::Uuid::new_v4().to_string();
    let store = BatonStore::open_hosted(&dir.path().join("baton.sqlite"), &host_id).unwrap();
    let claim = match acquire(&store, dir.path(), &host_id, &SystemProbe).unwrap() {
        ClaimOutcome::Claimed(claim) => claim,
        other => panic!("expected claim, got {other:?}"),
    };
    assert_eq!(claim.host_id, host_id);
    assert_eq!(store.get_host_claim().unwrap().as_ref(), Some(&claim));
    assert!(store.release_host_claim(&host_id).unwrap());
    assert!(store.get_host_claim().unwrap().is_none());
}
