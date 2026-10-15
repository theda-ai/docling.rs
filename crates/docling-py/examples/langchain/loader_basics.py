"""DoclingLoader basics: documents → LangChain ``Document``s, no LLM needed.

    pip install "docling-rs[langchain]"
    python loader_basics.py [file-or-url ...]

Declarative formats (DOCX, HTML, Markdown, XLSX, …) convert with no models;
PDFs and images need the one-time ``python -c "import docling_rs;
docling_rs.download_models()"``. The default chunker's tokenizer (~0.5 MB) is
fetched on first use.
"""

import sys

from docling_rs.langchain import DoclingLoader, ExportType

SOURCES = sys.argv[1:] or ["https://arxiv.org/pdf/2408.09869"]  # Docling Technical Report


def page_numbers(metadata):
    """Pages a chunk came from, out of docling's chunk metadata (``dl_meta``)
    — what a RAG answer cites."""
    pages = {
        prov["page_no"]
        for item in metadata["dl_meta"]["doc_items"]
        for prov in item.get("prov", [])
    }
    return sorted(pages)


# 1. One LangChain Document per chunk (the default, ExportType.DOC_CHUNKS):
#    docling's hybrid chunking — structure-aware, token-bounded (all-MiniLM-L6-v2,
#    256 tokens), each chunk prefixed with its heading path for embedding.
chunks = DoclingLoader(file_path=SOURCES).load()
print(f"{len(chunks)} chunks")
for doc in chunks[:3]:
    meta = doc.metadata["dl_meta"]
    print(f"\n--- {doc.metadata['source']} pages={page_numbers(doc.metadata)} headings={meta.get('headings')}")
    print(doc.page_content[:300])

# 2. One LangChain Document per input, as Markdown (ExportType.MARKDOWN) —
#    e.g. for a Markdown-aware text splitter, or for summarization.
whole = DoclingLoader(file_path=SOURCES, export_type=ExportType.MARKDOWN).load()
print(f"\n{len(whole)} Markdown document(s), {sum(len(d.page_content) for d in whole)} characters")

# 3. lazy_load() streams: each input is converted and chunked only as the
#    iterator reaches it — large batches never sit in memory at once.
for i, doc in enumerate(DoclingLoader(file_path=SOURCES).lazy_load()):
    if i == 2:
        break
