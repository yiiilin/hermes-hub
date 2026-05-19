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
    kind text not null check (kind in ('external', 'managed_docker')),
    status text not null default 'provisioning' check (status in ('provisioning', 'running', 'stopped', 'error')),
    name text not null,
    base_url text not null,
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
    provider_name text not null,
    provider_base_url text not null,
    provider_api_key_secret_ref text not null,
    default_model text not null,
    allowed_models jsonb not null default '[]'::jsonb,
    allow_streaming boolean not null default true,
    request_timeout_seconds integer not null default 60,
    is_active boolean not null default true,
    created_at timestamptz not null default now(),
    updated_at timestamptz not null default now()
);

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
