-- Independent literal result projection over the same current corpus and indexes.
-- Fixture spans are short ASCII strings; no excerpt truncation is needed here.
SELECT coalesce(jsonb_agg(result ORDER BY path,start_byte,evidence_id),'[]'::jsonb)
FROM (
  SELECT v.path,s.start_byte,s.evidence_id,
    jsonb_build_object('evidence_id',s.evidence_id,'version_id',s.version_id,
      'asset_id',v.asset_id,'path',v.path,'start_byte',s.start_byte,'end_byte',s.end_byte,
      'status','current','mode','literal','excerpt',s.text,'excerpt_truncated',false) AS result
  FROM bench_smoke.spans s
  JOIN bench_smoke.assets a ON a.current_version_id=s.version_id
  JOIN bench_smoke.versions v ON v.version_id=s.version_id
  WHERE strpos(s.text,'retrieval')>0
  ORDER BY v.path,s.start_byte,s.evidence_id
  LIMIT 10
) hits;
