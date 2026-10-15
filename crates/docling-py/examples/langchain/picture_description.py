"""Describe document pictures with any LangChain chat model that takes images
— the port of langchain-docling's picture-description example.

    pip install "docling-rs[langchain]" langchain-openai
    # a local vision model, e.g. LM Studio serving granite-vision-3.2-2b:
    export OPENAI_BASE_URL=http://localhost:1234/v1 OPENAI_API_KEY=none VISION_MODEL=granite-vision-3.2-2b
    python picture_description.py [file-or-url]

Each picture the engine extracted (PDF figures, DOCX/PPTX/HTML images) is
sent to the model; the answer lands in ``picture.meta.description`` and in the
Markdown export, so it is searchable text for RAG.
"""

import os
import sys

from langchain_openai import ChatOpenAI

from docling_rs import DocumentConverter, InputFormat, PdfFormatOption, PdfPipelineOptions
from docling_rs.langchain import PictureDescriptionLangChainOptions

SOURCE = sys.argv[1] if len(sys.argv) > 1 else "https://arxiv.org/pdf/2501.17887"
MODEL_ID = os.getenv("VISION_MODEL", "gpt-4o-mini")

llm = ChatOpenAI(model=MODEL_ID)

pipeline_options = PdfPipelineOptions(do_picture_description=True)
pipeline_options.picture_description_options = PictureDescriptionLangChainOptions(
    llm=llm,
    prompt="Describe the image in three sentences. Be concise and accurate.",
    provenance=MODEL_ID,
)
converter = DocumentConverter(
    format_options={InputFormat.PDF: PdfFormatOption(pipeline_options=pipeline_options)}
)
doc = converter.convert(SOURCE).document

for pic in doc.pictures[:5]:
    if pic.meta and pic.meta.description:
        print(f"{pic.self_ref} — caption: {pic.caption_text(doc=doc)!r}")
        print(f"  {pic.meta.description.created_by}: {pic.meta.description.text}\n")
