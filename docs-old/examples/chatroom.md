# Lattice ChatRoom Design (Milestone 7)

**Goal**: Build a decentralized, offline-first, IRC-like chat application to demonstrate Lattice as a generic Replicated State Machine (RSM) platform.

## Architecture

The ChatRoom application will demonstrate that Lattice is not just a Key-Value store, but a platform for any deterministic state machine. We will implement a custom `ChatState` optimized for message logs.

```mermaid
graph TD
    CLI[lattice-chat CLI]-->|commands| Node
    IRC[IRC Gateway]-->|TCP| Client[Standard IRC Client]
    IRC-->|commands| Node
    
    subgraph Node [Lattice Node]
        SM[StoreManager]
        SM-->RootStore[Root Store / Directory]
        SM-->Chan1[#general Store]
        SM-->Chan2[#dev Store]
        
        subgraph Stores
            Chan1-->|manages| CS1[ChatState]
            Chan2-->|manages| CS2[ChatState]
        end
    end
```

## 1. Data Model (`lattice-chat` crate)

We will create a new crate `lattice-chat` containing the state machine and application logic.

### 1.1 `ChatState` (StateMachine)

Instead of using `KvState`, we implement `StateMachine` directly to store an ordered log of messages. This is more efficient than KV for chat logs (no key storage overhead, optimized for "append & scan").

**State Structure:**
```rust
struct ChatState {
    // Ordered list of messages (by logical clock / causal order)
    messages: Vec<Message>,
    // Index for quick lookups (e.g. by ID for replies/reactions)
    index: HashMap<Hash, usize>,
}

struct Message {
    id: Hash,           // Intention DAG Intention Hash
    author: PubKey,
    timestamp: HLC,
    content: MessageType,
    reactions: HashMap<String, Vec<PubKey>>, // emoji -> authors
}

enum MessageType {
    Text(String),
    Reply { reply_to: Hash, text: String },
    // Edits could be handled here too
}
```

**Operations (Ops):**
- `Post(text)`: Append new message.
- `Reply(hash, text)`: Append reply (client renders threading).
- `React(hash, emoji)`: Update existing message state (CRDT map logic).

### 1.2 Channel Management (Root Store)

Channels are just valid Lattice Stores. We use the **Root Store** (which uses `KvState`) as the channel directory.

- **Key**: `/channels/{name}`
- **Value**: `{ "store_id": "uuid...", "created_at": "..." }`

Joining a channel `#general` involves:
1. Check root store for `/channels/#general`.
2. If exists, get `store_id` and open that store.
3. If not, create new store, register in root store.

## 2. CLI Interface

A new subcommand or standalone binary `lattice-chat`.

```bash
# Start the chat daemon/client
lattice chat init --name "Alice"

# List channels (from root store)
lattice chat list

# Join a channel (opens store, starts syncing)
lattice chat join #general

# Send message
lattice chat msg #general "Hello world!"

# TUI Mode (Interactive)
lattice chat tui
```

## 3. IRC Gateway (TCP Server)

To support existing clients (Textual, Irssi, WeeChat), we implement a translation layer.

- **Listen**: `127.0.0.1:6667`
- **Protocol**: subset of IRC (RFC 1459).
- **Mapping**:
    - `JOIN #channel` -> Open Store, Sync.
    - `PRIVMSG #channel :msg` -> `ChatState::apply(Post(msg))`.
    - `Topic` -> `ChatState` metadata.
    - `Names` -> List of peers (authors) seen in the Intention DAG.

## Implementation Plan

### Phase 1: The Chat Engine (`lattice-chat`)
- [ ] Define `ChatState` implementing `StateMachine`.
- [ ] Implement `apply()` logic for Post/Reply/React.
- [ ] Implement `ChatRoom` high-level API (wraps `Store<ChatState>`).

### Phase 2: Channel Directory
- [ ] Define schema for Root Store channel registry.
- [ ] Implement `ChannelManager` to handle join/leave/create flows.

### Phase 3: CLI
- [ ] Basic TUI using `ratatui` or simple readline loop.
- [ ] Watch mode for real-time updates.

### Phase 4: IRC Gateway
- [ ] Async TCP server handling IRC protocol.
- [ ] Map IRC commands onto Lattice ops.

## Technical Considerations

- **Causal Ordering**: Messages should be sorted by causal history (Lamport/Vector clocks implicit in Intention DAG). `ChatState` simply appends in apply order, which is causally consistent by definition of the Intention DAG.
- **Conflicting Reacts**: Reactions are a set. CRDT "Add Wins" logic is implicit since operations are additive.
- **Pruning**: Chat logs grow forever. Future work: `snapshot` and `truncate`.
