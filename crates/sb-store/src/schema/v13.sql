CREATE INDEX acquisitions_unpublished ON acquisitions(acquisition_id)
  WHERE object_state <> 'published';
CREATE INDEX d_search_docs_unusable ON d_search_docs(doc_id)
  WHERE state <> 'indexed' OR truncated_fields <> '[]';
CREATE INDEX d_search_docs_by_version ON d_search_docs(extractor_version);
