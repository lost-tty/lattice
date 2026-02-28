mod common;
use common::TestLogStore;
use lattice_logstore::proto::{AppendRequest, AppendResponse, ReadRequest, ReadResponse};
use lattice_store_base::invoke_command;

#[tokio::test]
async fn test_dispatch_log() {
    let store = TestLogStore::new();

    // 1. Append
    let req = AppendRequest {
        content: b"msg1".to_vec(),
    };
    let resp: AppendResponse = invoke_command(&store, "Append", req)
        .await
        .expect("Append failed");
    assert_eq!(resp.hash.len(), 32);

    // 2. Append second
    let req2 = AppendRequest {
        content: b"msg2".to_vec(),
    };
    invoke_command::<_, AppendResponse>(&store, "Append", req2)
        .await
        .expect("Append 2 failed");

    // 3. Read All
    let req_read = ReadRequest { tail: None };
    let resp_read: ReadResponse = invoke_command(&store, "Read", req_read)
        .await
        .expect("Read failed");

    assert_eq!(resp_read.entries.len(), 2);
    assert_eq!(resp_read.entries[0], b"msg1");
    assert_eq!(resp_read.entries[1], b"msg2");

    // 4. Read Tail
    let req_tail = ReadRequest { tail: Some(1) };
    let resp_tail: ReadResponse = invoke_command(&store, "Read", req_tail)
        .await
        .expect("Read tail failed");
    assert_eq!(resp_tail.entries.len(), 1);
    assert_eq!(resp_tail.entries[0], b"msg2");
}
