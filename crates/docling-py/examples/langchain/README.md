# LangChain examples — `docling_rs.langchain`

`DoclingLoader` is the port of docling's
[langchain-docling](https://github.com/docling-project/docling-langchain)
loader on the Rust engine: same API and output, no PyTorch.

```bash
pip install "docling-rs[langchain]"
python -c "import docling_rs; docling_rs.download_models()"   # PDFs/images only
```

| Script | What it shows | Extra deps |
|---|---|---|
| [`loader_basics.py`](./loader_basics.py) | chunks vs. Markdown export, page numbers and headings from `dl_meta`, lazy loading | — |
| [`rag_agent.py`](./rag_agent.py) | ingestion → `InMemoryVectorStore` → a LangChain agent with a retrieval tool that cites pages | `langchain langchain-openai` (any OpenAI-compatible server: `OPENAI_BASE_URL`) |
| [`picture_description.py`](./picture_description.py) | describing figures with a vision chat model during conversion | `langchain-openai` |

Every script takes file paths or URLs as arguments and defaults to an arXiv
paper. Coming from langchain-docling, the only change is the import:

```python
# was: from langchain_docling import DoclingLoader
from docling_rs.langchain import DoclingLoader
```
