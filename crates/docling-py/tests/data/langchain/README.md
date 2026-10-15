# langchain-docling fixtures

`dl_doc_1.json` (input) and `lc_doc_md_1.json` / `lc_doc_chunks_1.json`
(expected LangChain documents) are copied verbatim from
[docling-project/docling-langchain](https://github.com/docling-project/docling-langchain)
`test/data/` at commit `5e8f40369d0f724798899e3ba45844c4cd6fa57a`
(MIT License, Copyright (c) 2025 International Business Machines).
`tests/test_langchain.py` asserts that `docling_rs.langchain.DoclingLoader`
reproduces them exactly — the upstream loader's own test expectations.
