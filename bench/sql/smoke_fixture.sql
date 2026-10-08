-- Synthetic development fixture; not evidence of retrieval relevance or scale.
SET statement_timeout = '60s';
SELECT evidence.init_collection('bench_smoke', '{}'::jsonb);
DO $$
DECLARE
  i integer;
  aid uuid;
  vid uuid;
  body text;
BEGIN
  FOR i IN 1..1000 LOOP
    aid := md5(i::text)::uuid;
    body := 'retrieval fixture ' || lpad(i::text,5,'0') || ' citation source';
    PERFORM evidence.stage_version('bench_smoke',jsonb_build_object(
      'asset_id',aid,'path',lpad(i::text,5,'0') || '.txt',
      'source',body,'source_sha256',encode(sha256(convert_to(body,'UTF8')),'hex'),
      'spans',jsonb_build_array(jsonb_build_object('start_byte',0,'end_byte',octet_length(body))),
      'ingestion_key','fixture-' || i,'expected_revision',0));
    SELECT version_id INTO STRICT vid FROM bench_smoke.versions WHERE asset_id=aid;
    PERFORM evidence.publish_version('bench_smoke',jsonb_build_object('version_id',vid));
  END LOOP;
END $$;
ANALYZE bench_smoke.assets;
ANALYZE bench_smoke.versions;
ANALYZE bench_smoke.spans;
