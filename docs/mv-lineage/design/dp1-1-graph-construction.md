# DP1-1: Lineage Graph Construction

## Decision

Phase 1 will build the lineage graph in memory with `petgraph`. Phase 0 only
records this decision; it does not add `petgraph` as a dependency.

Stable external node identifiers and deterministic boundary ordering will be
maintained with `BTreeMap` and `BTreeSet`. Petgraph node indices and iteration
order are implementation details and must not become serialized identifiers or
report-order guarantees. Exports will sort nodes and edges before writing
deterministic JSON.

## Rationale

Expected lineage graphs contain hundreds to low thousands of nodes. An
in-memory Rust graph supports the required traversal, reachability, cycle
detection, and topological operations without adding a database, JavaScript or
WASM runtime, separate process, or persistence format. `petgraph` provides
well-tested graph structures and algorithms while preserving the signed
single-binary deployment model.

The rejected alternative was an embedded graph database such as Apache AGE on
PGlite. Its runtime and operational cost is not justified by the expected graph
size or current query requirements.

## Revisit Conditions

Reconsider an external graph store if graphs must persist across runs, require
interactive ad hoc queries, or grow beyond comfortable in-memory processing.
Any replacement must preserve deterministic exports and stable external node
identifiers.
