-- Keep the legacy agent-job tables available to older Codex binaries.
--
-- This migration remains in the history so databases that already recorded
-- version 42 continue to validate, but it intentionally performs no schema
-- change. Removing these tables would make the database incompatible with the
-- previous stable fork release.
SELECT 1;
