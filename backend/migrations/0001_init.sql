create table if not exists users (
    id uuid primary key,
    email text not null unique,
    password_hash text not null,
    role text not null check (role in ('admin', 'user')),
    status text not null default 'active' check (status in ('active', 'disabled')),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists sessions (
    id uuid primary key,
    user_id uuid not null references users(id) on delete cascade,
    session_token_hash text not null unique,
    expires_at timestamptz not null,
    created_at timestamptz not null default now()
);

create table if not exists invites (
    id uuid primary key,
    token_hash text not null unique,
    created_by_user_id uuid references users(id) on delete set null,
    status text not null default 'pending' check (status in ('pending', 'used', 'revoked', 'expired', 'exhausted')),
    expires_at timestamptz not null,
    max_uses integer not null check (max_uses > 0),
    used_count integer not null default 0 check (used_count >= 0),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists invite_uses (
    id uuid primary key,
    invite_id uuid not null references invites(id) on delete cascade,
    used_by_user_id uuid references users(id) on delete set null,
    used_at timestamptz not null default now(),
    ip_address text,
    user_agent text
);

create table if not exists hermes_instances (
    id uuid primary key,
    user_id uuid not null unique references users(id) on delete cascade,
    kind text not null check (kind in ('managed_docker')),
    status text not null default 'provisioning' check (status in ('provisioning', 'running', 'stopped', 'error')),
    name text not null,
    api_token_secret_ref text,
    container_id text,
    host_workspace_path text,
    host_sandbox_path text,
    host_config_path text,
    health_status text not null default 'unknown',
    last_health_check_at timestamptz,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);
-- external Hermes 模式已经删除，升级时直接清理旧实例；相关 channel 记录会按外键级联删除。
delete from hermes_instances where kind <> 'managed_docker';
alter table hermes_instances drop constraint if exists hermes_instances_kind_check;
alter table hermes_instances add constraint hermes_instances_kind_check
    check (kind in ('managed_docker'));
-- Adapter-only 之后 Hub 不再通过 Hermes inbound URL 访问容器，旧 base_url 运行时元数据可以直接删除。
alter table hermes_instances drop column if exists base_url;

create table if not exists instance_tokens (
    id uuid primary key,
    hermes_instance_id uuid not null references hermes_instances(id) on delete cascade,
    token_hash text not null unique,
    status text not null default 'active' check (status in ('active', 'revoked')),
    created_at timestamptz not null default now(),
    revoked_at timestamptz
);

create table if not exists model_configs (
    id uuid primary key,
    config_kind text not null default 'llm' check (config_kind in ('llm', 'image', 'title')),
    provider_name text not null,
    provider_base_url text not null,
    provider_api_key_secret_ref text not null,
    default_model text not null,
    allowed_models jsonb not null default '[]'::jsonb,
    api_type text not null default 'chat_completions' check (api_type in ('chat_completions', 'responses', 'images_generations')),
    reasoning_effort text check (reasoning_effort in ('minimal', 'low', 'medium', 'high')),
    allow_streaming boolean not null default true,
    request_timeout_seconds integer not null default 60,
    is_active boolean not null default true,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

alter table model_configs add column if not exists config_kind text not null default 'llm';
alter table model_configs add column if not exists api_type text not null default 'chat_completions';
alter table model_configs add column if not exists reasoning_effort text;

create table if not exists proxy_audit_logs (
    id uuid primary key,
    user_id uuid references users(id) on delete set null,
    hermes_instance_id uuid references hermes_instances(id) on delete set null,
    direction text not null check (direction in ('browser_to_hermes', 'hermes_to_llm')),
    method text not null,
    path text not null,
    status_code integer,
    duration_ms integer,
    error_code text,
    created_at timestamptz not null default now()
);

create table if not exists llm_usage_events (
    id uuid primary key,
    user_id uuid references users(id) on delete set null,
    hermes_instance_id uuid references hermes_instances(id) on delete set null,
    model text not null,
    upstream_provider text not null,
    status_code integer,
    duration_ms integer,
    prompt_tokens integer,
    completion_tokens integer,
    total_tokens integer,
    created_at timestamptz not null default now()
);

create table if not exists system_settings (
    key text primary key,
    value text not null,
    updated_at timestamptz not null default now()
);

-- 系统设置必须有稳定默认值，老部署升级后不需要管理员手动补配置。
insert into system_settings (key, value)
values ('max_sessions_per_user', '20')
on conflict (key) do nothing;

create table if not exists channels (
    id uuid primary key,
    user_id uuid not null references users(id) on delete cascade,
    hermes_instance_id uuid not null references hermes_instances(id) on delete cascade,
    name text not null,
    description text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    unique (user_id, name)
);

create table if not exists channel_sessions (
    id uuid primary key,
    channel_id uuid not null references channels(id) on delete cascade,
    kind text not null check (kind in ('chat', 'agent')),
    hermes_session_id text,
    hermes_response_id text,
    hermes_run_id text,
    title text,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

create table if not exists channel_session_messages (
    id uuid primary key,
    session_id uuid not null references channel_sessions(id) on delete cascade,
    role text not null check (role in ('user', 'assistant')),
    client_message_key text,
    content text not null,
    attachments jsonb not null default '[]'::jsonb,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

alter table channel_session_messages add column if not exists client_message_key text;
alter table channel_session_messages add column if not exists updated_at timestamptz;
update channel_session_messages set updated_at = created_at where updated_at is null;
alter table channel_session_messages alter column updated_at set default now();
alter table channel_session_messages alter column updated_at set not null;
create unique index if not exists channel_session_messages_client_key_idx
    on channel_session_messages(session_id, client_message_key)
    where client_message_key is not null;

create table if not exists channel_attachments (
    id uuid primary key,
    session_id uuid not null references channel_sessions(id) on delete cascade,
    message_id uuid references channel_session_messages(id) on delete set null,
    direction text not null check (direction in ('input', 'output')),
    bucket text not null,
    object_key text not null unique,
    name text not null,
    content_type text not null,
    size_bytes bigint not null check (size_bytes >= 0),
    kind text not null check (kind in ('file', 'image')),
    created_at timestamptz not null default now()
);

create table if not exists channel_runs (
    id uuid primary key,
    session_id uuid not null references channel_sessions(id) on delete cascade,
    user_message_id uuid references channel_session_messages(id) on delete set null,
    status text not null default 'queued' check (
        status in ('queued', 'leased', 'running', 'completed', 'failed', 'cancelled', 'expired')
    ),
    input text not null default '',
    input_attachments jsonb not null default '[]'::jsonb,
    output_message_id uuid references channel_session_messages(id) on delete set null,
    error text,
    lease_expires_at timestamptz,
    attempt_count integer not null default 0 check (attempt_count >= 0),
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now(),
    completed_at timestamptz
);

create unique index if not exists channel_runs_user_message_idx
    on channel_runs(user_message_id)
    where user_message_id is not null;

create index if not exists channel_runs_ready_idx
    on channel_runs(session_id, created_at)
    where status in ('queued', 'leased', 'running');

-- 保持现有 API 语义：用户可以先创建 channel，再绑定或创建 Hermes 实例。
alter table channels alter column hermes_instance_id drop not null;
