use super::*;

/// A malformed name glob is also `INVALID_PARAMS` (the requester's own
/// compiler diagnostic).
#[tokio::test]
async fn fs_search_invalid_glob_is_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///", "a[b"))
        .await
        .expect_err("broken glob");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("expected Rpc INVALID_PARAMS, got {other:?}"),
    }
}
