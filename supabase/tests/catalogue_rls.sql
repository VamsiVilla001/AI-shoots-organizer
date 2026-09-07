begin;

create extension if not exists pgtap with schema extensions;
select plan(13);

insert into auth.users (id, email)
values
  ('00000000-0000-0000-0000-000000000001', 'owner@skwad.test'),
  ('00000000-0000-0000-0000-000000000002', 'editor@skwad.test'),
  ('00000000-0000-0000-0000-000000000003', 'viewer@skwad.test'),
  ('00000000-0000-0000-0000-000000000004', 'revoked@skwad.test');

insert into public.workspaces (id, kind, name, owner_id)
values (
  '10000000-0000-0000-0000-000000000001',
  'personal',
  'RLS test workspace',
  '00000000-0000-0000-0000-000000000001'
);

insert into public.workspace_members (workspace_id, user_id, role, revoked_at)
values
  ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000002', 'editor', null),
  ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000003', 'viewer', null),
  ('10000000-0000-0000-0000-000000000001', '00000000-0000-0000-0000-000000000004', 'viewer', now());

insert into public.libraries (id, workspace_id, label)
values (
  '20000000-0000-0000-0000-000000000001',
  '10000000-0000-0000-0000-000000000001',
  'NAS library'
);

insert into public.shoots (id, workspace_id, library_id, name)
values (
  '30000000-0000-0000-0000-000000000001',
  '10000000-0000-0000-0000-000000000001',
  '20000000-0000-0000-0000-000000000001',
  'Test shoot'
);

insert into public.catalogue_revisions (
  id, package_id, workspace_id, shoot_id, revision_number, state,
  object_key, ciphertext_blake3, created_by, published_at
)
values
  (
    '40000000-0000-0000-0000-000000000001',
    '50000000-0000-0000-0000-000000000001',
    '10000000-0000-0000-0000-000000000001',
    '30000000-0000-0000-0000-000000000001',
    1,
    'published',
    '10000000-0000-0000-0000-000000000001/50000000-0000-0000-0000-000000000001/40000000-0000-0000-0000-000000000001.skwad',
    'published-hash',
    '00000000-0000-0000-0000-000000000001',
    now()
  ),
  (
    '40000000-0000-0000-0000-000000000002',
    '50000000-0000-0000-0000-000000000002',
    '10000000-0000-0000-0000-000000000001',
    '30000000-0000-0000-0000-000000000001',
    2,
    'draft',
    null,
    null,
    '00000000-0000-0000-0000-000000000001',
    null
  );

set local role anon;
select is((select count(*) from public.workspaces), 0::bigint, 'anonymous users see no workspaces');
reset role;

set local role authenticated;
select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-000000000001', true);
select is((select count(*) from public.workspaces), 1::bigint, 'owner sees the workspace');
select is((select count(*) from public.catalogue_revisions), 2::bigint, 'owner sees drafts and published revisions');

select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-000000000002', true);
select is((select count(*) from public.workspaces), 1::bigint, 'editor sees the workspace');
select is((select count(*) from public.catalogue_revisions), 2::bigint, 'editor sees drafts and published revisions');
select throws_ok(
  $$update public.catalogue_revisions
      set state='published', object_key='forbidden.skwad', published_at=now()
      where id='40000000-0000-0000-0000-000000000002'$$,
  'editor cannot publish a revision'
);

select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-000000000003', true);
select is((select count(*) from public.workspaces), 1::bigint, 'viewer sees the workspace');
select is((select count(*) from public.catalogue_revisions), 1::bigint, 'viewer sees published revisions only');
select throws_ok(
  $$insert into public.catalogue_revisions
      (id,package_id,workspace_id,shoot_id,revision_number,state,created_by)
    values
      ('40000000-0000-0000-0000-000000000003','50000000-0000-0000-0000-000000000003',
       '10000000-0000-0000-0000-000000000001','30000000-0000-0000-0000-000000000001',3,
       'draft','00000000-0000-0000-0000-000000000003')$$,
  'viewer cannot create a draft revision'
);

select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-000000000004', true);
select is((select count(*) from public.workspaces), 0::bigint, 'revoked member sees no workspace');
select is(
  public.workspace_role_for('10000000-0000-0000-0000-000000000001'),
  null::public.workspace_role,
  'revoked member has no effective role'
);

select set_config('request.jwt.claim.sub', '00000000-0000-0000-0000-000000000001', true);
select lives_ok(
  $$update public.catalogue_revisions
      set state='published',
          object_key='10000000-0000-0000-0000-000000000001/50000000-0000-0000-0000-000000000002/40000000-0000-0000-0000-000000000002.skwad',
          ciphertext_blake3='owner-published-hash',
          published_at=now()
      where id='40000000-0000-0000-0000-000000000002'$$,
  'owner can publish a draft revision'
);
select throws_ok(
  $$update public.catalogue_revisions
      set ciphertext_blake3='mutated'
      where id='40000000-0000-0000-0000-000000000001'$$,
  'published revisions are immutable'
);

select * from finish();
rollback;
