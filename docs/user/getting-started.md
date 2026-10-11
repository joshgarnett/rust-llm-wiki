# Create a wiki and find your first fact

Start by [installing lwiki](install.md). Then follow the steps below to save a note
and find information in it. You can do this on macOS or Linux without an API key
or an internet connection.

A guided setup route that saves wiki, embedding/authentication and agent settings
is requested but not implemented. Until that route is validated, use the current
manual commands below. They save a default usable in a new terminal; optional
remote providers and host installation remain separate explicit choices.

~~~sh
DEMO="$(mktemp -d)"
WIKI="$DEMO/Personal Wiki"
lwiki --offline init "$WIKI" --set-default
mkdir "$WIKI/notes"
cat > "$WIKI/notes/deployment.md" <<'NOTE'
# Atlas deployment

The Atlas service uses Postgres for its primary database.
Run atlas_migrate before deploying a new release.
Rollback restores the previous release and keeps the database unchanged.
NOTE
lwiki wiki show
lwiki --offline read --path notes/deployment.md --max-bytes 6000
lwiki --offline --dry-run source add "$WIKI/notes/deployment.md" --title "Atlas deployment"
lwiki --offline source add "$WIKI/notes/deployment.md" --title "Atlas deployment"
lwiki --offline index sync
lwiki --offline search 'atlas_migrate' --mode literal --limit 5
lwiki --offline context 'Atlas deployment' --max-bytes 6000 --max-tokens 1500
~~~

Check that context contains `atlas_migrate` and its captured Source/Revision
citation. A title match alone does not answer the question. Context returns
evidence, rather than generating an answer. Warnings and omissions describe
bounds; a citation authenticates retained bytes, not the truth of a claim.

The original note stays editable. Capture returns a managed Source path and
immutable Revision paths; keep those separate from the original. Init creates
an editable WIKI.md landing page linking Pages and Sources. It does not adopt
other notes. The saved default lets a later process resume from another directory.

Open the folder in Obsidian or another Markdown editor. When you change the
original, [refresh its capture explicitly](everyday-use.md#capture-and-refresh-notes).
Use [everyday use](everyday-use.md) for wiki selection, existing folders, exact-byte
edits and authored Pages. For a missing fact, follow [retrieval](retrieval.md);
for damaged metadata or backups, use [recovery](recovery.md). Run `lwiki --offline
check` for strict health rather than assuming a successful context checked every
file.
