# WebAssembly Consensus Bus Architecture

> **Status**: Future Vision  
> Transform Lattice from a passive Distributed Key-Value Store into a **Replicated State Machine** (a Distributed Computer) where the log carries generic instructions instead of just database writes.

---

## 1. Protocol Layer Upgrade

**Objective:** Decouple the log from the specific logic of "putting" and "deleting."

- Deprecate the hardcoded `Operation` enum (`Put`/`Delete`)
- Introduce a generic `Instruction` payload:

```protobuf
message Instruction {
  string program_id = 1;  // E.g., "sys" (native), "zfs" (wasm), "ai_job"
  string method = 2;      // E.g., "put", "resize_image", "snapshot"
  bytes args = 3;         // Serialized arguments
}
```

**Why:** This allows the log to carry any computation, not just storage updates.

---

## 2. Execution Layer Evolution (StoreActor)

**Objective:** Turn the `StoreActor` from a "Database Writer" into a "VM Scheduler."

### Dispatcher
Instead of calling `db.insert()` directly, route entries based on `program_id`.

### WASM Runtime Integration
- Integrate a WASM runtime (e.g., `wasmtime`)
- The dispatcher loads the WASM binary referenced by the contract ID
- Execute the `method` with `args`

### Host Functions (Syscalls)
The WASM guest needs functions to read/write state:
- `host_db_get()`
- `host_db_put()`

> **Crucial:** Pass the active `redb` transaction into the WASM context so all side effects are atomic.

---

## 3. The "System" Contract (Legacy Support)

**Objective:** Preserve current high-performance file syncing without overhead.

- Create a "Native" Contract (ID: `"sys"`)
- When `program_id == "sys"`, bypass WASM entirely
- Run the existing Rust code for Put/Delete directly against redb

**Result:** Simple file metadata operations remain blazing fast (O(1)) and backward compatible.

---

## 4. Trusted Delegation Model (The "Personal Cloud")

**Objective:** Allow diverse hardware (Phones vs. Desktops) to cooperate.

### Job Splitting
- **Entry 1:** `JobRequest` (Created by Phone, ignored by Phone)
- **Entry 2:** `JobResult` (Created by Desktop, applied by Phone)

### Access Control (ACLs)
Implement in the System Contract:
> "Only accept writes to `/processed_photos/*` if signed by the `Desktop_GPU_Key`."

---

## 5. CAS & ZFS Implementation

**Objective:** Move file logic out of the kernel and into the cluster.

### CAS Policy in WASM
- Instead of hardcoded I/O, the WASM contract decides:
  - **Where** data goes (Erasure Coding vs. Replication)
  - **How** it is stored (Compression/Encryption)

### Streaming Host Functions
- Read 4KB at a time to prevent large files from crashing WASM RAM

---

## Implementation Phases

- [ ] **Phase 1 (Protocol):** Update Protobuf to support `Instruction` payload alongside legacy Ops
- [ ] **Phase 2 (Dispatcher):** Refactor `StoreActor` to route messages instead of applying them
- [ ] **Phase 3 (Runtime):** Embed `wasmtime` and define the `LatticeHost` trait (`db_read`/`db_write`)
- [ ] **Phase 4 (System):** Port existing redb logic to a "native" handler behind the Dispatcher
- [ ] **Phase 5 (Pilot):** Deploy first WASM contract (e.g., a simple "Counter" or "Tagging" bot) to test the pipeline
