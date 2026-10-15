# Regenerates checkbox_glyphs.pdf (#609): `python make_checkbox_glyphs.py checkbox_glyphs.pdf`
# with ReportLab and DejaVu Sans (the ballot boxes need a font that has them).
import sys
from reportlab.lib.pagesizes import letter
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.ttfonts import TTFont
from reportlab.pdfgen import canvas
pdfmetrics.registerFont(TTFont("DejaVu", "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf"))
pdfmetrics.registerFont(TTFont("DejaVuB", "/usr/share/fonts/truetype/dejavu/DejaVuSans-Bold.ttf"))
c = canvas.Canvas(sys.argv[1], pagesize=letter)
c.setTitle("Checklist")
y = 720
c.setFont("DejaVuB", 18); c.drawString(78, y, "Shopping checklist"); y -= 34
c.setFont("DejaVu", 11)
for line in ["Tick what you already have at home before going to the shop, and",
             "leave the rest unticked so it ends up on the list."]:
    c.drawString(78, y, line); y -= 15
y -= 14
for box, label in [("☐", "Milk"), ("☒", "Eggs"), ("☑", "Bread"), ("☐", "Butter")]:
    c.drawString(96, y, f"{box}  {label}"); y -= 20
y -= 14
for line in ["Everything still unticked goes into the basket. Bring a bag; the shop",
             "charges for them."]:
    c.drawString(78, y, line); y -= 15
c.showPage(); c.save()
