# rsmc-engine: Database Schema

PostgreSQL, created by the files in `migrations/` and applied automatically on
startup when `auto_migrate` is enabled (the default). Migrations are embedded in
the binary via `sqlx::migrate!`, so the running service needs only database
connectivity, no separate migration step. The `pgcrypto` extension (for
`gen_random_uuid()`) is created by the first migration.

## Enums

| Type                | Values                                                        |
|---------------------|---------------------------------------------------------------|
| `user_role`         | `admin`, `member`, `guest`                                    |
| `member_role`       | `owner`, `admin`, `member`                                    |
| `channel_type`      | `public`, `private`, `direct`, `group`                        |
| `notification_kind` | `mention`, `direct_message`, `channel_invite`, `thread_reply` |

## Tables

### `users`
Account records. Passwords are Argon2id hashes, never plaintext.

- `id UUID PK` (default `gen_random_uuid()`)
- `email`, `username`, `display_name`
- `password_hash` (Argon2id)
- `role user_role`
- `avatar_url` (nullable)
- `is_active BOOL`
- `created_at`, `updated_at` (trigger-touched)
- `status_emoji`, `status_text`, `status_expires_at` (nullable; custom status, expired status treated as absent by the API)
- `avatar_path`, `avatar_content_type` (nullable), `avatar_version INT` (uploaded-avatar storage + cache-busting version)

Unique, case-insensitive indexes on `LOWER(email)` and `LOWER(username)`.

### `refresh_tokens`
One row per issued refresh token, enabling rotation and revocation. Only a hash
(fingerprint) is stored.

- `id UUID PK`
- `user_id -> users(id)` `ON DELETE CASCADE`
- `token_hash` (unique fingerprint)
- `expires_at`, `revoked BOOL`, `created_at`

Index on `user_id`.

### `channels`
Channels and DMs. `name`/`topic` are null for direct channels.

- `id UUID PK`
- `name` (nullable), `topic` (nullable)
- `channel_type channel_type`
- `created_by -> users(id)` `ON DELETE RESTRICT`
- `created_at`, `updated_at` (trigger-touched; also bumped on new messages)
- `archived_at` (nullable; set = archived/read-only), `archived_by -> users(id)` `ON DELETE SET NULL`

Partial unique index on `LOWER(name)` where `name IS NOT NULL` (named channels
are unique; DMs aren't constrained).

### `channel_members`
Membership join table with per-channel role, read cursor, and mute flag.

- `channel_id -> channels(id)` `ON DELETE CASCADE`
- `user_id -> users(id)` `ON DELETE CASCADE`
- `role member_role`
- `last_read_at` (nullable; advanced when history is fetched)
- `muted BOOL` (per-member notification mute)
- `joined_at`
- **PK** `(channel_id, user_id)`

Index on `user_id` (for "my channels").

### `messages`
Messages and thread replies. Soft-deleted (content cleared, `deleted_at` set).

- `id UUID PK`
- `channel_id -> channels(id)` `ON DELETE CASCADE`
- `user_id -> users(id)` `ON DELETE RESTRICT`
- `content TEXT`
- `parent_id -> messages(id)` `ON DELETE CASCADE` (null = root; set = reply)
- `reply_count INT`
- `edited_at`, `deleted_at` (nullable), `created_at`

Indexes: `(channel_id, created_at DESC, id DESC)` for keyset history; partial
index on `parent_id` for thread lookups.

### `scheduled_messages`
Messages queued for later delivery. The background sweeper claims due rows
(`FOR UPDATE SKIP LOCKED`), inserts a real message, and stamps `sent_at`.

- `id UUID PK`
- `channel_id -> channels(id)` `ON DELETE CASCADE`
- `user_id -> users(id)` `ON DELETE CASCADE`
- `content TEXT`, `parent_id -> messages(id)` `ON DELETE CASCADE` (nullable)
- `scheduled_for`, `sent_at` (nullable), `created_at`

Partial indexes (both where `sent_at IS NULL`): on `scheduled_for` for the due
scan, and on `(channel_id, user_id)` for listing pending items.

### `drafts`
One retained unsent draft per user per channel. Saving upserts; clearing deletes.

- `user_id -> users(id)` `ON DELETE CASCADE`
- `channel_id -> channels(id)` `ON DELETE CASCADE`
- `content TEXT`, `updated_at`
- **PK** `(user_id, channel_id)`

### `attachments`
Uploaded files. `channel_id` is null until the file is bound to a message. Bytes
live on disk (`storage_path`); this table holds metadata.

- `id UUID PK`
- `uploader_id -> users(id)` `ON DELETE RESTRICT`
- `channel_id -> channels(id)` `ON DELETE SET NULL` (nullable)
- `filename`, `content_type`, `size_bytes`
- `storage_path` (server-local; never serialized to clients)
- `created_at`

Index on `channel_id`.

### `message_attachments`
Join table linking messages to attachments.

- `message_id -> messages(id)` `ON DELETE CASCADE`
- `attachment_id -> attachments(id)` `ON DELETE CASCADE`
- **PK** `(message_id, attachment_id)`

### `notifications`
Per-user notifications with a JSON payload.

- `id UUID PK`
- `user_id -> users(id)` `ON DELETE CASCADE`
- `kind notification_kind`
- `payload JSONB`
- `read_at` (nullable), `created_at`

Indexes: `(user_id, created_at DESC)`; partial index on `user_id` where
`read_at IS NULL` (fast unread counts).

### `presence`
Last-seen / online state, one row per user. Live online status also comes from
the in-memory hub; this table backs "last seen" across restarts.

- `user_id UUID PK -> users(id)` `ON DELETE CASCADE`
- `online BOOL`, `last_seen`

### `webhooks`
Outbound webhook subscriptions. `channel_id` null = all channels visible to the
owner. `secret` HMAC-signs deliveries and is never serialized back.

- `id UUID PK`
- `owner_id -> users(id)` `ON DELETE CASCADE`
- `channel_id -> channels(id)` `ON DELETE CASCADE` (nullable)
- `target_url`
- `events TEXT[]` (e.g. `{message.created,message.deleted}`)
- `secret`, `is_active BOOL`, `created_at`

Indexes: on `channel_id`; partial index on `is_active` where `is_active`.

## Migrations

Files run in lexical order at startup:

- `0001_init.sql`: core schema (enums, users, tokens, channels, members, messages, attachments, notifications, presence, triggers).
- `0002`..`0005`: app links, webhooks, and the `channel_members.muted` flag.
- `0006_lifecycle_and_status.sql`: channel archival columns, user status columns, and the `scheduled_messages` and `drafts` tables.
- `0007_avatar_upload.sql`: `avatar_path`, `avatar_content_type`, `avatar_version` on `users`.

`0006` and `0007` are additive and nullable, so they apply safely to an existing
database.

## Triggers

`set_updated_at()` keeps `updated_at` current on `users` and `channels`.

## Design notes

- **Soft deletes** on messages preserve thread structure and reply counts.
- **Keyset pagination** (not OFFSET) keeps history queries fast at any depth.
- **Cascade rules are deliberate:** deleting a user cascades their memberships, tokens, notifications, drafts, scheduled messages, and presence, but message/channel authorship uses `RESTRICT`, so deactivate users (`is_active = false`) instead of deleting them.
- **Extending the schema:** add `migrations/000N_description.sql`; files run in lexical order at startup.
