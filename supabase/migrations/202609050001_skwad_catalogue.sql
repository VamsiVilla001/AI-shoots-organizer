create type public.workspace_kind as enum ('personal', 'organisation');
create type public.workspace_role as enum ('owner', 'editor', 'viewer');
create type public.revision_state as enum ('draft', 'published');

create table public.workspaces (
  id uuid primary key default gen_random_uuid(),
  kind public.workspace_kind not null default 'personal',
  name text not null check (length(name) between 1 and 120),
  owner_id uuid not null references auth.users(id) on delete cascade,
  created_at timestamptz not null default now()
);

create table public.workspace_members (
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  user_id uuid not null references auth.users(id) on delete cascade,
  role public.workspace_role not null,
  revoked_at timestamptz,
  created_at timestamptz not null default now(),
  primary key (workspace_id, user_id)
);

create table public.devices (
  id uuid primary key,
  user_id uuid not null references auth.users(id) on delete cascade,
  opaque_key_id text not null unique,
  hpke_public_key text not null,
  label text,
  revoked_at timestamptz,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz not null default now()
);

create table public.libraries (
  id uuid primary key,
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  label text not null,
  created_at timestamptz not null default now(),
  unique (workspace_id, id)
);

create table public.shoots (
  id uuid primary key,
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  library_id uuid not null,
  name text not null,
  notes text,
  cloud_revision bigint not null default 0,
  tombstone boolean not null default false,
  updated_at timestamptz not null default now(),
  unique (workspace_id, id),
  unique (workspace_id, id, library_id),
  foreign key (workspace_id, library_id)
    references public.libraries(workspace_id, id) on delete cascade
);

-- Only normalized, metadata-only records. No absolute roots, originals,
-- thumbnails, proxies, crops or embeddings are accepted by this schema.
create table public.media_metadata (
  id uuid primary key,
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  shoot_id uuid not null,
  library_id uuid not null,
  normalized_relative_path text not null check (
    normalized_relative_path <> '' and
    normalized_relative_path not like '/%' and
    normalized_relative_path not like '%..%' and
    normalized_relative_path not like '%:\\%' and
    normalized_relative_path not like '%://%'
  ),
  filename text not null,
  media_type text not null check (media_type in ('photo', 'video')),
  file_size bigint not null check (file_size >= 0),
  width integer, height integer, captured_at timestamptz,
  duration double precision, fps double precision, bitrate bigint,
  video_codec text, audio_codec text,
  recognition_state text, recognition_confidence real,
  face_geometry jsonb not null default '[]'::jsonb,
  tags jsonb not null default '[]'::jsonb,
  rating smallint not null default 0 check (rating between 0 and 5),
  pick_state text not null default 'none' check (pick_state in ('none', 'pick', 'reject')),
  cloud_revision bigint not null default 0,
  tombstone boolean not null default false,
  unique (library_id, normalized_relative_path),
  foreign key (workspace_id, shoot_id, library_id)
    references public.shoots(workspace_id, id, library_id) on delete cascade
);

create table public.catalogue_revisions (
  id uuid primary key,
  package_id uuid not null,
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  shoot_id uuid not null,
  revision_number bigint not null check (revision_number > 0),
  state public.revision_state not null default 'draft',
  object_key text,
  ciphertext_blake3 text,
  created_by uuid not null references auth.users(id),
  created_at timestamptz not null default now(),
  published_at timestamptz,
  unique(package_id, revision_number),
  unique(shoot_id, revision_number),
  foreign key (workspace_id, shoot_id)
    references public.shoots(workspace_id, id) on delete cascade,
  check ((state = 'draft' and object_key is null and published_at is null) or
         (state = 'published' and object_key is not null and published_at is not null))
);

create table public.revision_recipients (
  revision_id uuid not null references public.catalogue_revisions(id) on delete cascade,
  device_id uuid not null references public.devices(id) on delete cascade,
  opaque_key_id text not null,
  wrapped_key jsonb not null,
  created_at timestamptz not null default now(),
  primary key (revision_id, device_id)
);

create table public.invitations (
  id uuid primary key default gen_random_uuid(),
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  email text not null,
  role public.workspace_role not null check (role <> 'owner'),
  invited_by uuid not null references auth.users(id),
  accepted_at timestamptz,
  revoked_at timestamptz,
  created_at timestamptz not null default now()
);

create table public.notifications (
  id bigint generated always as identity primary key,
  user_id uuid not null references auth.users(id) on delete cascade,
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  revision_id uuid references public.catalogue_revisions(id) on delete cascade,
  kind text not null,
  read_at timestamptz,
  created_at timestamptz not null default now()
);

create table public.sync_cursors (
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  user_id uuid not null references auth.users(id) on delete cascade,
  device_id uuid not null references public.devices(id) on delete cascade,
  cursor bigint not null default 0,
  updated_at timestamptz not null default now(),
  primary key (workspace_id, device_id)
);

create table public.audit_history (
  id bigint generated always as identity primary key,
  workspace_id uuid not null references public.workspaces(id) on delete cascade,
  actor_id uuid references auth.users(id) on delete set null,
  action text not null,
  entity_type text not null,
  entity_id uuid,
  occurred_at timestamptz not null default now()
);

create or replace function public.notify_published_revision()
returns trigger language plpgsql security definer set search_path = public
as $$
begin
  if old.state='draft' and new.state='published' then
    insert into public.notifications(user_id,workspace_id,revision_id,kind)
      select user_id,new.workspace_id,new.id,'revision_published'
      from public.workspace_members where workspace_id=new.workspace_id and revoked_at is null and user_id<>new.created_by;
    insert into public.audit_history(workspace_id,actor_id,action,entity_type,entity_id)
      values(new.workspace_id,new.created_by,'publish','catalogue_revision',new.id);
  end if;
  return new;
end $$;

create or replace function public.workspace_role_for(target_workspace uuid)
returns public.workspace_role language sql stable security definer set search_path = public
as $$ select role from public.workspace_members where workspace_id = target_workspace and user_id = auth.uid() and revoked_at is null $$;

create or replace function public.add_workspace_owner_membership()
returns trigger language plpgsql security definer set search_path = public
as $$ begin insert into public.workspace_members(workspace_id,user_id,role) values(new.id,new.owner_id,'owner'); return new; end $$;
create trigger add_workspace_owner after insert on public.workspaces for each row execute function public.add_workspace_owner_membership();

create or replace function public.prevent_published_revision_mutation()
returns trigger language plpgsql as $$
begin
  if old.state = 'published' then raise exception 'published revisions are immutable'; end if;
  return new;
end $$;
create trigger immutable_published_revisions before update on public.catalogue_revisions for each row execute function public.prevent_published_revision_mutation();
create trigger notify_published_revision after update on public.catalogue_revisions for each row execute function public.notify_published_revision();

alter table public.workspaces enable row level security;
alter table public.workspace_members enable row level security;
alter table public.devices enable row level security;
alter table public.libraries enable row level security;
alter table public.shoots enable row level security;
alter table public.media_metadata enable row level security;
alter table public.catalogue_revisions enable row level security;
alter table public.revision_recipients enable row level security;
alter table public.invitations enable row level security;
alter table public.notifications enable row level security;
alter table public.sync_cursors enable row level security;
alter table public.audit_history enable row level security;

create policy workspace_read on public.workspaces for select to authenticated using (public.workspace_role_for(id) is not null);
create policy workspace_create on public.workspaces for insert to authenticated with check (owner_id=auth.uid() and kind='personal');
create policy workspace_owner_write on public.workspaces for update to authenticated using (public.workspace_role_for(id) = 'owner') with check (public.workspace_role_for(id) = 'owner');
create policy workspace_owner_delete on public.workspaces for delete to authenticated using (public.workspace_role_for(id) = 'owner');
create policy members_read on public.workspace_members for select to authenticated using (public.workspace_role_for(workspace_id) is not null);
create policy members_owner_write on public.workspace_members for all to authenticated using (public.workspace_role_for(workspace_id) = 'owner') with check (public.workspace_role_for(workspace_id) = 'owner');
create policy own_devices on public.devices for all to authenticated using (user_id = auth.uid()) with check (user_id = auth.uid());
create policy library_read on public.libraries for select to authenticated using (public.workspace_role_for(workspace_id) is not null);
create policy library_edit on public.libraries for all to authenticated using (public.workspace_role_for(workspace_id) in ('owner','editor')) with check (public.workspace_role_for(workspace_id) in ('owner','editor'));
create policy shoot_read on public.shoots for select to authenticated using (public.workspace_role_for(workspace_id) is not null);
create policy shoot_edit on public.shoots for all to authenticated using (public.workspace_role_for(workspace_id) in ('owner','editor')) with check (public.workspace_role_for(workspace_id) in ('owner','editor'));
create policy metadata_read on public.media_metadata for select to authenticated using (public.workspace_role_for(workspace_id) is not null);
create policy metadata_edit on public.media_metadata for all to authenticated using (public.workspace_role_for(workspace_id) in ('owner','editor')) with check (public.workspace_role_for(workspace_id) in ('owner','editor'));
create policy revision_read on public.catalogue_revisions for select to authenticated using (public.workspace_role_for(workspace_id) is not null and (state='published' or public.workspace_role_for(workspace_id) in ('owner','editor')));
create policy revision_draft_insert on public.catalogue_revisions for insert to authenticated with check (created_by=auth.uid() and state='draft' and public.workspace_role_for(workspace_id) in ('owner','editor'));
create policy revision_draft_update on public.catalogue_revisions for update to authenticated using (state='draft' and public.workspace_role_for(workspace_id) in ('owner','editor')) with check ((state='draft' and public.workspace_role_for(workspace_id) in ('owner','editor')) or (state='published' and public.workspace_role_for(workspace_id)='owner'));
create policy recipient_read on public.revision_recipients for select to authenticated using (device_id in (select id from public.devices where user_id=auth.uid()));
create policy invitation_owner on public.invitations for all to authenticated using (public.workspace_role_for(workspace_id)='owner') with check (public.workspace_role_for(workspace_id)='owner');
create policy own_notifications on public.notifications for select to authenticated using (user_id=auth.uid());
create policy own_cursor on public.sync_cursors for all to authenticated using (user_id=auth.uid()) with check (user_id=auth.uid());
create policy audit_read on public.audit_history for select to authenticated using (public.workspace_role_for(workspace_id)='owner');

insert into storage.buckets (id, name, public, file_size_limit, allowed_mime_types)
values ('skwad-packages', 'skwad-packages', false, 536870912, array['application/vnd.skwad.catalogue'])
on conflict (id) do nothing;

create policy package_storage_read on storage.objects for select to authenticated
using (
  bucket_id='skwad-packages' and exists (
    select 1 from public.catalogue_revisions revision
    where revision.object_key = name
      and revision.workspace_id = ((storage.foldername(name))[1])::uuid
      and public.workspace_role_for(revision.workspace_id) is not null
      and (
        revision.state = 'published' or
        public.workspace_role_for(revision.workspace_id) in ('owner','editor')
      )
  )
);
create policy package_storage_owner_insert on storage.objects for insert to authenticated
with check (bucket_id='skwad-packages' and public.workspace_role_for(((storage.foldername(name))[1])::uuid)='owner');

alter publication supabase_realtime add table public.notifications;
