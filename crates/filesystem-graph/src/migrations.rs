/// Current durable graph schema version.
pub const GRAPH_SCHEMA_VERSION: u32 = 5;

pub(crate) const MIGRATION_001: &str = r"
CREATE TABLE IF NOT EXISTS graph_schema_migrations (
    version INTEGER PRIMARY KEY,
    description TEXT NOT NULL
) STRICT;

CREATE TABLE IF NOT EXISTS graph_volumes (
    volume_id BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    display_name TEXT,
    mount_points_json TEXT NOT NULL,
    filesystem TEXT,
    removable INTEGER NOT NULL CHECK(removable IN (0, 1)),
    local INTEGER NOT NULL CHECK(local IN (0, 1)),
    state INTEGER NOT NULL CHECK(state BETWEEN 0 AND 2),
    reconciliation_reason INTEGER,
    generation INTEGER NOT NULL DEFAULT 0 CHECK(generation >= 0)
) STRICT;

CREATE TABLE IF NOT EXISTS graph_provider_checkpoints (
    volume_id BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    provider_id TEXT NOT NULL,
    format_version INTEGER NOT NULL CHECK(format_version >= 0),
    opaque BLOB NOT NULL,
    FOREIGN KEY(volume_id) REFERENCES graph_volumes(volume_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE IF NOT EXISTS graph_file_objects (
    volume_id BLOB NOT NULL CHECK(length(volume_id) = 16),
    file_id BLOB NOT NULL CHECK(length(file_id) = 16),
    kind INTEGER NOT NULL CHECK(kind BETWEEN 0 AND 4),
    size INTEGER NOT NULL CHECK(size >= 0),
    created_at_unix_ms INTEGER,
    modified_at_unix_ms INTEGER,
    hidden INTEGER NOT NULL CHECK(hidden IN (0, 1)),
    availability INTEGER NOT NULL CHECK(availability BETWEEN 0 AND 2),
    tombstoned INTEGER NOT NULL DEFAULT 0 CHECK(tombstoned IN (0, 1)),
    PRIMARY KEY(volume_id, file_id),
    FOREIGN KEY(volume_id) REFERENCES graph_volumes(volume_id) ON DELETE CASCADE
) WITHOUT ROWID, STRICT;

CREATE TABLE IF NOT EXISTS graph_file_links (
    file_link_id BLOB PRIMARY KEY NOT NULL CHECK(length(file_link_id) = 16),
    volume_id BLOB NOT NULL CHECK(length(volume_id) = 16),
    file_id BLOB NOT NULL CHECK(length(file_id) = 16),
    parent_volume_id BLOB CHECK(parent_volume_id IS NULL OR length(parent_volume_id) = 16),
    parent_file_id BLOB CHECK(parent_file_id IS NULL OR length(parent_file_id) = 16),
    name TEXT NOT NULL,
    traversal_boundary INTEGER NOT NULL DEFAULT 0 CHECK(traversal_boundary IN (0, 1)),
    CHECK((parent_volume_id IS NULL) = (parent_file_id IS NULL)),
    FOREIGN KEY(volume_id, file_id) REFERENCES graph_file_objects(volume_id, file_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX IF NOT EXISTS graph_file_links_object
    ON graph_file_links(volume_id, file_id, file_link_id);
CREATE INDEX IF NOT EXISTS graph_file_links_parent
    ON graph_file_links(parent_volume_id, parent_file_id);
CREATE UNIQUE INDEX IF NOT EXISTS graph_one_exact_name_per_parent
    ON graph_file_links(parent_volume_id, parent_file_id, name)
    WHERE parent_volume_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS graph_one_exact_root_name_per_volume
    ON graph_file_links(volume_id, name)
    WHERE parent_volume_id IS NULL;

CREATE TABLE IF NOT EXISTS graph_path_refresh_jobs (
    job_id INTEGER PRIMARY KEY,
    volume_id BLOB NOT NULL CHECK(length(volume_id) = 16),
    root_file_id BLOB NOT NULL CHECK(length(root_file_id) = 16),
    enqueued_generation INTEGER NOT NULL CHECK(enqueued_generation >= 0),
    state INTEGER NOT NULL DEFAULT 0 CHECK(state BETWEEN 0 AND 2),
    FOREIGN KEY(volume_id, root_file_id) REFERENCES graph_file_objects(volume_id, file_id) ON DELETE CASCADE
) STRICT;

CREATE UNIQUE INDEX IF NOT EXISTS graph_one_pending_refresh_per_root
    ON graph_path_refresh_jobs(volume_id, root_file_id) WHERE state = 0;

INSERT OR IGNORE INTO graph_schema_migrations(version, description)
VALUES (1, 'platform-neutral volume/object/link graph and provider checkpoints');
PRAGMA user_version = 1;
";

pub(crate) const MIGRATION_002: &str = r"
ALTER TABLE graph_path_refresh_jobs ADD COLUMN projection_scan_cursor BLOB;

CREATE TABLE graph_catalog_documents (
    document_id BLOB PRIMARY KEY NOT NULL CHECK(length(document_id) = 16),
    volume_id BLOB NOT NULL CHECK(length(volume_id) = 16),
    file_id BLOB NOT NULL CHECK(length(file_id) = 16),
    file_link_id BLOB NOT NULL UNIQUE CHECK(length(file_link_id) = 16),
    document_version INTEGER NOT NULL CHECK(document_version >= 0),
    document_json TEXT NOT NULL,
    FOREIGN KEY(volume_id, file_id) REFERENCES graph_file_objects(volume_id, file_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX graph_catalog_documents_object
    ON graph_catalog_documents(volume_id, file_id);

CREATE TABLE graph_projection_outbox (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    document_id BLOB NOT NULL CHECK(length(document_id) = 16),
    document_version INTEGER NOT NULL CHECK(document_version >= 0),
    mutation_json TEXT NOT NULL
) STRICT;

CREATE INDEX graph_projection_outbox_document
    ON graph_projection_outbox(document_id, sequence);

CREATE TABLE graph_projection_consumers (
    consumer_id TEXT PRIMARY KEY NOT NULL,
    last_sequence INTEGER NOT NULL CHECK(last_sequence >= 0),
    index_generation INTEGER NOT NULL CHECK(index_generation >= 0)
) STRICT;

INSERT INTO graph_schema_migrations(version, description)
VALUES (2, 'transactional catalog desired state, projection outbox, and consumer checkpoints');
PRAGMA user_version = 2;
";

pub(crate) const MIGRATION_003: &str = r"
ALTER TABLE graph_catalog_documents ADD COLUMN projection_path TEXT;
ALTER TABLE graph_catalog_documents ADD COLUMN projection_fingerprint BLOB
    CHECK(projection_fingerprint IS NULL OR length(projection_fingerprint) = 32);

INSERT INTO graph_schema_migrations(version, description)
VALUES (3, 'compact desired catalog payload with normalized graph metadata');
PRAGMA user_version = 3;
";

pub(crate) const MIGRATION_004: &str = r"
CREATE TABLE graph_volume_projection_refresh_jobs (
    volume_id BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    enqueued_generation INTEGER NOT NULL CHECK(enqueued_generation >= 0),
    projection_scan_cursor BLOB,
    state INTEGER NOT NULL DEFAULT 0 CHECK(state IN (0, 2)),
    FOREIGN KEY(volume_id) REFERENCES graph_volumes(volume_id) ON DELETE CASCADE
) WITHOUT ROWID, STRICT;

CREATE INDEX graph_catalog_documents_volume_link
    ON graph_catalog_documents(volume_id, file_link_id);
CREATE INDEX graph_file_links_volume_link
    ON graph_file_links(volume_id, file_link_id);

INSERT INTO graph_schema_migrations(version, description)
VALUES (4, 'bounded volume-wide projection refresh queue');
PRAGMA user_version = 4;
";

pub(crate) const MIGRATION_005: &str = r"
ALTER TABLE graph_file_objects ADD COLUMN observation_generation INTEGER NOT NULL DEFAULT 0
    CHECK(observation_generation >= 0);
ALTER TABLE graph_file_links ADD COLUMN observation_generation INTEGER NOT NULL DEFAULT 0
    CHECK(observation_generation >= 0);

CREATE TABLE graph_volume_observation_sessions (
    volume_id BLOB PRIMARY KEY NOT NULL CHECK(length(volume_id) = 16),
    scan_generation INTEGER NOT NULL CHECK(scan_generation >= 0),
    scan_mode INTEGER NOT NULL CHECK(scan_mode IN (0, 1)),
    phase INTEGER NOT NULL CHECK(phase BETWEEN 0 AND 2),
    final_provider_id TEXT,
    final_format_version INTEGER CHECK(final_format_version IS NULL OR final_format_version >= 0),
    final_checkpoint BLOB,
    CHECK(
      (phase = 0 AND final_provider_id IS NULL AND final_format_version IS NULL
                 AND final_checkpoint IS NULL)
      OR
      (phase IN (1, 2) AND final_provider_id IS NOT NULL AND final_format_version IS NOT NULL
                         AND final_checkpoint IS NOT NULL)
    ),
    FOREIGN KEY(volume_id) REFERENCES graph_volumes(volume_id) ON DELETE CASCADE
) WITHOUT ROWID, STRICT;

CREATE INDEX graph_file_links_observation_sweep
    ON graph_file_links(volume_id, observation_generation, file_link_id);
CREATE INDEX graph_file_objects_observation_sweep
    ON graph_file_objects(volume_id, observation_generation, file_id)
    WHERE tombstoned = 0;

INSERT INTO graph_schema_migrations(version, description)
VALUES (5, 'crash-safe volume observation sessions and bounded snapshot sweep');
PRAGMA user_version = 5;
";
