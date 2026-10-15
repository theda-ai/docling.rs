"""Agentic RAG over documents: DoclingLoader → vector store → a LangChain agent
with a retrieval tool that cites pages.

    pip install "docling-rs[langchain]" langchain langchain-openai
    export OPENAI_API_KEY=...          # or any OpenAI-compatible server:
    export OPENAI_BASE_URL=http://localhost:1234/v1 CHAT_MODEL=... EMBED_MODEL=...
    python rag_agent.py "Which AI models does Docling use?"

The ingestion side is all docling.rs: conversion (Rust, no PyTorch) and
docling's hybrid chunking with page provenance per chunk. Swap the chat model,
embeddings or vector store for any LangChain integration.
"""

import os
import sys

from langchain.agents import create_agent
from langchain_core.tools import tool
from langchain_core.vectorstores import InMemoryVectorStore
from langchain_openai import ChatOpenAI, OpenAIEmbeddings

from docling_rs.langchain import DoclingLoader

SOURCES = ["https://arxiv.org/pdf/2408.09869"]  # Docling Technical Report
QUESTION = " ".join(sys.argv[1:]) or "Which AI models does Docling use, and what do they do?"

# --- ingest ------------------------------------------------------------------
chunks = DoclingLoader(file_path=SOURCES).load()
for doc in chunks:
    # Flatten docling's chunk metadata to what the agent should cite.
    dl_meta = doc.metadata.pop("dl_meta")
    doc.metadata["pages"] = sorted(
        {p["page_no"] for item in dl_meta["doc_items"] for p in item.get("prov", [])}
    )
    doc.metadata["headings"] = " > ".join(dl_meta.get("headings") or [])

embeddings = OpenAIEmbeddings(model=os.getenv("EMBED_MODEL", "text-embedding-3-small"))
store = InMemoryVectorStore.from_documents(chunks, embeddings)
print(f"indexed {len(chunks)} chunks from {len(SOURCES)} document(s)")


# --- agent ---------------------------------------------------------------------
@tool
def search_documents(query: str) -> str:
    """Search the indexed documents; returns passages with their source,
    section and page numbers."""
    hits = store.similarity_search(query, k=4)
    return "\n\n".join(
        f"[{d.metadata['source']} p.{','.join(map(str, d.metadata['pages']))}"
        f" | {d.metadata['headings']}]\n{d.page_content}"
        for d in hits
    )


agent = create_agent(
    model=ChatOpenAI(model=os.getenv("CHAT_MODEL", "gpt-4o-mini")),
    tools=[search_documents],
    system_prompt=(
        "Answer from the documents only: search them (more than once if needed) "
        "and cite the page numbers you used."
    ),
)
result = agent.invoke({"messages": [{"role": "user", "content": QUESTION}]})
print(result["messages"][-1].content)
