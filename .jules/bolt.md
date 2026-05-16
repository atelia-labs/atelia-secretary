## 2024-05-18 - Deferring struct clones in iterators
**Learning:** Found a performance bottleneck in data access patterns. In `crates/atelia-core/src/store.rs`, iterators fetching data like `project_status_snapshot` and `query_job_events` were eager to clone entire structs `JobRecord`, `PolicyDecision`, etc., before sorting, truncating, and paginating. This lead to heavy, unnecessary materialization of large amounts of records which were then immediately discarded.
**Action:** When filtering, sorting, and paginating large collections of data inside `InMemoryStore`, defer cloning by operating entirely on references. Collect the filtered references into a `Vec`, sort/truncate, and only map `.cloned()` immediately before collecting the final result.

## 2024-05-18 - Avoid full materialization during pagination
**Learning:** In the core store `query_job_events`, using `collect_filtered_job_events` retrieves all events that match a filter, meaning potentially thousands of records are pulled into memory at once just to page through a small subset using `page_records`.
**Action:** When filtering with an eventual limit (like a paginated endpoint), track skipped items and stop retaining once the current page reaches a finite `page_size`. If `page_size` is omitted, keep the filtered stream unbounded instead of capping retained records.

## 2026-05-15 - Defer struct cloning during memory store sorting
**Learning:** Found another instance of the performance anti-pattern described in a previous journal entry. The `list_schema_migrations` function in `InMemoryStore` (`crates/atelia-core/src/store.rs`) was cloning the entire HashMap of `SchemaMigrationRecord` structs before performing a multi-level sort. This eagerly allocates memory and causes heavy struct swapping during sort instead of light reference swaps.
**Action:** Defer struct cloning in collections before sorting by iterating `.values()`, sorting a collection of references (`Vec<&T>`), and only cloning using `.cloned().collect()` at the end.

## 2024-05-18 - Avoid full materialization during JobQuery pagination
**Learning:** In the core store `query_jobs`, pulling all filtered jobs into memory using `.collect::<Vec<_>>()` and then sorting using `.sort_by()` defeated the purpose of pagination. If there are tens of thousands of jobs, retrieving page 1 size 10 still eagerly clones all 10,000 matches into an allocated vector before picking out the first 10.
**Action:** By converting `InMemoryInner.jobs` from a `HashMap` to a `BTreeMap`, iterators implicitly sort by `JobId`. We can lazily process filters via iterators and pass the stream to `page_records` directly, stopping iteration gracefully after extracting the 10 needed items.
