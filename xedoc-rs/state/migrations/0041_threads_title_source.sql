ALTER TABLE threads ADD COLUMN title_source TEXT NOT NULL DEFAULT 'derived';

UPDATE threads
SET title_source = 'manual'
WHERE TRIM(title) <> ''
  AND (
    (TRIM(first_user_message) <> '' AND TRIM(title) <> TRIM(first_user_message))
    OR (TRIM(first_user_message) = '' AND TRIM(preview) <> '' AND TRIM(title) <> TRIM(preview))
  );
