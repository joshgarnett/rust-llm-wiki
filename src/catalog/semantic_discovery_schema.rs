//! Experimental disposable discovery state. Canonical mutations invalidate it atomically.
pub(super) const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS semantic_discovery_state(
 space TEXT NOT NULL, policy TEXT NOT NULL, incarnation TEXT NOT NULL,
 required_seq INTEGER NOT NULL CHECK(required_seq>0),
 eligible_units INTEGER NOT NULL CHECK(eligible_units>=0),
 manifest_json TEXT, PRIMARY KEY(space,policy));
CREATE TABLE IF NOT EXISTS semantic_discovery_stages(
 space TEXT NOT NULL, policy TEXT NOT NULL, incarnation TEXT NOT NULL,
 required_seq INTEGER NOT NULL CHECK(required_seq>0),
 stage_json TEXT NOT NULL, PRIMARY KEY(space,policy));
CREATE TABLE IF NOT EXISTS semantic_discovery_audits(
 space TEXT NOT NULL, policy TEXT NOT NULL, incarnation TEXT NOT NULL,
 required_seq INTEGER NOT NULL CHECK(required_seq>0),
 identity_json TEXT NOT NULL, expected_digest TEXT NOT NULL,
 next_node INTEGER NOT NULL CHECK(next_node>=0), prefix_digest TEXT NOT NULL,
 last_key_json TEXT NOT NULL, cursor_json TEXT NOT NULL,
 PRIMARY KEY(space,policy));
"#;
