# dbxctl Design Nomenclature and Glossary

This reference defines the identifiers, abbreviations, and technical terms used
throughout dbxctl design documents, implementation plans, findings, and source
comments. It applies across the entire project.

Identifiers are case-sensitive. Uppercase identifiers generally refer to
design artifacts, requirements, assertions, or questions. Lowercase
identifiers generally refer to executable checks or command-facing names.

## Phases

| Abbreviation | Meaning |
| --- | --- |
| `P0`, Phase 0 | Discovery and de-risking. Resolve open technical questions and validate assumptions before building dependent functionality. |
| `P1`, Phase 1, and later | Successive implementation and delivery stages defined by the relevant roadmap or design document. |

Phase tags may appear in document titles, issue titles, and source comments.
Their exact entry and exit criteria belong in the roadmap for the feature being
built.

## Item Families

| Identifier | Meaning | Canonical location |
| --- | --- | --- |
| `D1`, `D2`, … | Product or architecture decisions. | The relevant design document's decisions section. |
| `DP<phase>-<number>` | A numbered decision proposal raised during a project phase. | The phase findings or dedicated decision record. |
| `V1`, `V2`, … | Questions or assumptions that must be verified before dependent code is written. | The relevant design document's verification section. |
| `v1`, `v2`, … | Executable checks that gather evidence for correspondingly numbered `V` items. | Probe implementation and probe documentation. |
| Named checks | Executable checks whose stable identifier is a descriptive name rather than a number. | Probe implementation and probe documentation. |
| `R1`, `R2`, … | Numbered requirements. | The relevant requirements document. |
| `T0`, `T1`, … | Numbered implementation or operational tasks. | The relevant task plan. |
| `A1`, `A2`, … | Runtime assertions or hard gates. | The relevant design's assertion section. |
| `G1`, `G2`, … | Gotchas inherited from earlier implementation or investigation work. | The relevant design's gotchas section. |
| `E1`, `E2`, … | Numbered errors or failure cases. | The relevant findings or gotchas section. |
| `N1`, `N2`, … | New findings or gotchas discovered after the initial design. | The relevant findings or gotchas section. |
| `[N-name]` | A named finding without a numeric identifier. | Source tags and the relevant findings document. |
| `Q1`, `Q2`, … | Numbered queries used by an implementation. | The relevant query reference. |
| `Thm <number>` | A property that a model must satisfy and that the implementation checks. | The relevant model or invariants section. |
| `§<number>` | A numbered section in a design document. | The design document that owns the section. |
| `§I.<number>` | A numbered section in an implementation reference. | The implementation reference that owns the section. |

### Identifier Rules

- Do not reuse an identifier for a different meaning within the same design.
- Preserve identifier case in code, documentation, issue titles, and reports.
- Treat a `V` item and its lowercase `v` check as related but distinct: one is
  a question, while the other is an evidence-gathering operation.
- A `DP` identifier belongs to the decision-proposal sequence for its phase; it
  is not interchangeable with a `D`, `V`, or `v` identifier.
- Define any new identifier family in this document before using it broadly.
- Keep the canonical definition in version-controlled documentation. Issues
  may discuss or track it, but should link back to that definition.

## Technical Acronyms

| Abbreviation | Meaning |
| --- | --- |
| API | Application programming interface. |
| CLI | Command-line interface. |
| CRUD | Create, read, update, and delete. |
| DAB, bundle | Databricks Asset Bundle. |
| DAG | Directed acyclic graph. |
| DBSQL | Databricks SQL. |
| FQN | Fully qualified name, represented as `Fqn` in Rust code. |
| MV | Materialized view. |
| PAT | Personal access token. |
| SCC | Strongly connected component. |
| SCD | Slowly changing dimension. |
| ST | Streaming table. |
| TUT | Table update trigger. |
| UC | Unity Catalog. |
| WCC | Weakly connected component. |

## Maintenance

Add terminology here when it is used across multiple project areas. Keep
feature-local terms in the owning design document, and link to them rather than
duplicating their definitions here.
