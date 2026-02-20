# Lattice RPC API

The daemon (`latticed`) exposes a gRPC API over a Unix Domain Socket (UDS) for CLI and GUI clients.

## Transport

- **Socket**: `{data_dir}/latticed.sock` (uses `DataDir` platform handling)
- **Protocol**: gRPC over UDS (tonic)
- **Auth**: Local socket permissions (same user)

---

## CLI Command Coverage

| CLI Command            | RPC Service          | RPC Method     | Status   |
|------------------------|----------------------|----------------|----------|
| `node status`          | NodeService          | GetStatus      | ✅       |
| `node set-name`        | NodeService          | SetName        | ✅       |
| `store create`         | StoreManager         | CreateStore    | ✅       |
| `store list`           | StoreManager         | ListStores     | ✅       |
| `store use`            | —                    | —              | CLI-only |
| `store status`         | StoreManager         | GetStoreStatus | ✅       |
| `peer join`            | NetworkService       | Join           | ✅       |
| `peer list`            | NetworkService       | ListPeers      | ✅       |
| `peer invite`          | NetworkService       | Invite         | ✅       |
| `peer revoke`          | NetworkService       | Revoke         | ✅       |
| `store create`         | StoreService         | Create         | ✅       |
| `store list`           | StoreService         | List           | ✅       |
| `store use`            | —                    | —              | CLI-only |
| `store delete`         | StoreService         | Delete         | ✅       |
| `store status`         | StoreService         | GetStatus      | ✅       |
| `store sync`           | StoreService         | Sync           | ✅       |
| `store debug`          | StoreService         | Debug          | ✅       |
| `store history`        | StoreService         | History        | ✅       |
| `store author-state`   | StoreService         | AuthorState    | ✅       |
| `get/put/delete/list`  | DynamicStoreService  | Exec           | ✅       |

---

## Services

### NodeService

See [`daemon.proto`](../lattice-rpc/proto/daemon.proto) for full message definitions.

```protobuf
service NodeService {
  rpc GetStatus(Empty) returns (NodeStatus);
  rpc SetName(SetNameRequest) returns (Empty);
  rpc Subscribe(Empty) returns (stream NodeEvent);
}
```

### NetworkService & StoreManager
Networking is handled by the `NetworkService`, while local store orchestration is handled by the `StoreManager` (following the flattened M10 architecture).

```protobuf
service NetworkService {
  rpc Join(JoinToken) returns (JoinResponse);
  rpc Invite(StoreId) returns (InviteToken);
  rpc ListPeers(StoreId) returns (PeerList);
  rpc Revoke(RevokeRequest) returns (Empty);
}

service StoreManager {
  rpc CreateStore(CreateStoreRequest) returns (StoreInfo);
  rpc ListStores(Empty) returns (StoreList);
  rpc GetStoreStatus(StoreId) returns (StoreInfo);
}
```

### StoreService

See [`daemon.proto`](../lattice-rpc/proto/daemon.proto) for full message definitions.

```protobuf
service StoreService {
  rpc Create(CreateStoreRequest) returns (StoreInfo);
  rpc List(MeshId) returns (StoreList);
  rpc GetStatus(StoreId) returns (StoreInfo);
  rpc Delete(StoreId) returns (Empty);
  rpc Sync(StoreId) returns (SyncResult);
  rpc Debug(StoreId) returns (DebugInfo);
  rpc History(HistoryRequest) returns (HistoryResponse);
  rpc AuthorState(AuthorStateRequest) returns (AuthorStateResponse);
  rpc OrphanCleanup(StoreId) returns (CleanupResult);
}
```

### DynamicStoreService

See [`daemon.proto`](../lattice-rpc/proto/daemon.proto) for full message definitions.

```protobuf
service DynamicStoreService {
  rpc Exec(ExecRequest) returns (ExecResponse);
  rpc GetDescriptor(StoreId) returns (DescriptorResponse);
  rpc ListMethods(StoreId) returns (MethodList);
}
```

---

## Implementation Status

| Phase | Description | Status |
|-------|-------------|--------|
| M7C-1 | UDS listener + NodeService | ✅ Done |
| M7C-2 | StoreManager RPCs | ✅ Done |
| M7C-3 | StoreService RPCs | ✅ Done |
| M7C-4 | DynamicStoreService | ✅ Done |
| M7D | CLI as RPC client | ✅ Done |
| M7E | Event Streaming | ✅ Done |

---

## Proto Location

`lattice-rpc/proto/daemon.proto`
