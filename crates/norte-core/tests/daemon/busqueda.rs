use super::*;

/// Un glob de nombre malformado también es `INVALID_PARAMS` (diagnóstico del
/// compilador del propio requester).
#[tokio::test]
async fn fs_search_glob_invalido_es_invalid_params() {
    let d = spawn_daemon(None).await;
    let c = connected_client(&d).await;
    let err = c
        .call::<_, FsTaskResult>(methods::FS_SEARCH, &search_by_name("mem:///", "a[b"))
        .await
        .expect_err("glob roto");
    match err {
        ClientError::Rpc(rpc) => assert_eq!(rpc.code, codes::INVALID_PARAMS),
        other => panic!("esperaba Rpc INVALID_PARAMS, fue {other:?}"),
    }
}
