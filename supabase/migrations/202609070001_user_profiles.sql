create table public.profiles (
  user_id uuid primary key references auth.users(id) on delete cascade,
  display_name text not null check (length(trim(display_name)) between 1 and 80),
  avatar_url text check (avatar_url is null or length(avatar_url) <= 2048),
  job_title text check (job_title is null or length(job_title) <= 120),
  organisation text check (organisation is null or length(organisation) <= 160),
  location text check (location is null or length(location) <= 120),
  bio text check (bio is null or length(bio) <= 500),
  created_at timestamptz not null default now(),
  updated_at timestamptz not null default now()
);

create or replace function public.create_profile_for_new_user()
returns trigger language plpgsql security definer set search_path = public
as $$
begin
  insert into public.profiles (user_id, display_name)
  values (
    new.id,
    coalesce(
      nullif(trim(new.raw_user_meta_data ->> 'display_name'), ''),
      nullif(split_part(coalesce(new.email, ''), '@', 1), ''),
      'SKWAD user'
    )
  )
  on conflict (user_id) do nothing;
  return new;
end
$$;

create trigger create_profile_after_signup
after insert on auth.users
for each row execute function public.create_profile_for_new_user();

-- Give accounts created before this migration a profile as well.
insert into public.profiles (user_id, display_name)
select
  id,
  coalesce(
    nullif(trim(raw_user_meta_data ->> 'display_name'), ''),
    nullif(split_part(coalesce(email, ''), '@', 1), ''),
    'SKWAD user'
  )
from auth.users
on conflict (user_id) do nothing;

create or replace function public.touch_profile_updated_at()
returns trigger language plpgsql set search_path = public
as $$
begin
  new.updated_at = now();
  return new;
end
$$;

create trigger touch_profile_updated_at
before update on public.profiles
for each row execute function public.touch_profile_updated_at();

alter table public.profiles enable row level security;

create policy profile_read_own on public.profiles
for select to authenticated
using (user_id = auth.uid());

create policy profile_update_own on public.profiles
for update to authenticated
using (user_id = auth.uid())
with check (user_id = auth.uid());

grant select, update on public.profiles to authenticated;
