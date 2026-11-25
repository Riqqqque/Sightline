# Draws the app icon options and packs each one as PNG and ICO, plus one comparison sheet.
# Everything is drawn here with GDI+; nothing is downloaded.
#
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\make-icons.ps1
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\make-icons.ps1 -Pick A
#
# -Pick copies one option to assets\icon\sightline.ico and sightline.png (the files the build uses).
param(
    [string]$OutDir,
    [ValidateSet('', 'A', 'B', 'C', 'D')][string]$Pick = ''
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
if (-not $OutDir) { $OutDir = Join-Path $root 'assets\icon\options' }
[void](New-Item -ItemType Directory -Force -Path $OutDir)

Add-Type -ReferencedAssemblies System.Drawing -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Drawing2D;
using System.Drawing.Imaging;
using System.IO;

public static class IconKit
{
    static Color C(string hex, int alpha)
    {
        Color c = ColorTranslator.FromHtml(hex);
        return Color.FromArgb(alpha, c.R, c.G, c.B);
    }
    static Color C(string hex) { return C(hex, 255); }

    const string White = "#eef1f5";
    const string Blue = "#6db3ff";
    const string Teal = "#43c9b6";
    const string Coral = "#ef6673";

    static GraphicsPath Round(float x, float y, float w, float h, float r)
    {
        var p = new GraphicsPath();
        float d = r * 2;
        p.AddArc(x, y, d, d, 180, 90);
        p.AddArc(x + w - d, y, d, d, 270, 90);
        p.AddArc(x + w - d, y + h - d, d, d, 0, 90);
        p.AddArc(x, y + h - d, d, d, 90, 90);
        p.CloseFigure();
        return p;
    }

    static void Dot(Graphics g, float cx, float cy, float r, Color c)
    {
        using (var b = new SolidBrush(c)) g.FillEllipse(b, cx - r, cy - r, r * 2, r * 2);
    }

    // The dark rounded square every option sits on. All drawing uses a 256-unit canvas.
    // At taskbar sizes the square fills the whole icon, so the picture inside gets every pixel.
    static void Tile(Graphics g, bool small)
    {
        float inset = small ? 2 : 10, side = 256 - 2 * inset, r = small ? 60 : 56;
        using (var p = Round(inset, inset, side, side, r))
        using (var b = new LinearGradientBrush(new PointF(0, inset), new PointF(0, 256 - inset), C("#202838"), C("#0c0f15")))
            g.FillPath(b, p);
        using (var p = Round(inset + 1.5f, inset + 1.5f, side - 3, side - 3, r - 1.5f))
        using (var pen = new Pen(C("#ffffff", small ? 44 : 30), 3))
            g.DrawPath(pen, p);
    }

    // Grows the picture around the middle of the canvas.
    static void Grow(Graphics g, float by)
    {
        g.TranslateTransform(128, 128);
        g.ScaleTransform(by, by);
        g.TranslateTransform(-128, -128);
    }

    // A: a field of view. Someone looking, what they see, and a target on the line of sight.
    static void Cone(Graphics g, bool small)
    {
        float ox = 66, oy = 128;
        using (var b = new LinearGradientBrush(new PointF(ox - 2, 0), new PointF(226, 0), C(Blue, 235), C(Blue, small ? 70 : 28)))
            g.FillPie(b, ox - 156, oy - 156, 312, 312, -27, 54);
        using (var pen = new Pen(C(White, 235), small ? 14 : 9))
        {
            pen.StartCap = LineCap.Round; pen.EndCap = LineCap.Round;
            g.DrawLine(pen, ox, oy, 190, oy);
        }
        Dot(g, 194, oy, small ? 22 : 17, C(Coral));
        Dot(g, ox, oy, small ? 30 : 26, C(White));
    }

    // B: two rosters side by side, mine and theirs, with my own row picked out.
    static void Rosters(Graphics g, bool small)
    {
        float w = 74, h = 36, gap = 16, top = 128 - (3 * h + 2 * gap) / 2;
        for (int i = 0; i < 3; i++)
        {
            float y = top + i * (h + gap);
            using (var p = Round(48, y, w, h, 12))
            using (var b = new SolidBrush(i == 0 ? C(White) : C(Teal)))
                g.FillPath(b, p);
            using (var p = Round(134, y, w, h, 12))
            using (var b = new SolidBrush(C(Coral)))
                g.FillPath(b, p);
        }
    }

    // C: a viewfinder locked on a point.
    static void Finder(Graphics g, bool small)
    {
        float a = 58, b = 198, arm = small ? 46 : 42;
        using (var pen = new Pen(C(White), small ? 24 : 20))
        {
            pen.StartCap = LineCap.Round; pen.EndCap = LineCap.Round; pen.LineJoin = LineJoin.Round;
            float[][] corners = {
                new float[] { a, a, 1, 1 }, new float[] { b, a, -1, 1 },
                new float[] { a, b, 1, -1 }, new float[] { b, b, -1, -1 } };
            foreach (var c in corners)
                g.DrawLines(pen, new[] {
                    new PointF(c[0], c[1] + c[3] * arm), new PointF(c[0], c[1]), new PointF(c[0] + c[2] * arm, c[1]) });
        }
        Dot(g, 128, 128, small ? 30 : 26, C(Blue));
    }

    // D: the letter S, drawn as one stroke.
    static void Letter(Graphics g, bool small)
    {
        float r = 38, cx = 128;
        using (var path = new GraphicsPath())
        using (var brush = new LinearGradientBrush(new PointF(0, 40), new PointF(0, 216), C(Blue), C(Teal)))
        using (var pen = new Pen(brush, small ? 34 : 30))
        {
            pen.StartCap = LineCap.Round; pen.EndCap = LineCap.Round; pen.LineJoin = LineJoin.Round;
            path.AddArc(cx - r, 90 - r, r * 2, r * 2, -35, -235);
            path.AddArc(cx - r, 166 - r, r * 2, r * 2, 270, 235);
            g.DrawPath(pen, path);
        }
    }

    public static Bitmap Render(string id, int px)
    {
        int ss = px >= 256 ? 4 : 8;
        int big = px * ss;
        var result = new Bitmap(px, px, PixelFormat.Format32bppArgb);
        using (var canvas = new Bitmap(big, big, PixelFormat.Format32bppPArgb))
        {
            using (var g = Graphics.FromImage(canvas))
            {
                g.SmoothingMode = SmoothingMode.AntiAlias;
                g.PixelOffsetMode = PixelOffsetMode.HighQuality;
                g.ScaleTransform(big / 256f, big / 256f);
                bool small = px <= 24;
                Tile(g, small);
                Grow(g, small ? 1.24f : px <= 48 ? 1.1f : 1f);
                switch (id)
                {
                    case "A": Cone(g, small); break;
                    case "B": Rosters(g, small); break;
                    case "C": Finder(g, small); break;
                    case "D": Letter(g, small); break;
                    default: throw new ArgumentException("unknown option " + id);
                }
            }
            using (var g = Graphics.FromImage(result))
            using (var attrs = new ImageAttributes())
            {
                attrs.SetWrapMode(WrapMode.TileFlipXY);
                g.CompositingMode = CompositingMode.SourceCopy;
                g.InterpolationMode = InterpolationMode.HighQualityBicubic;
                g.PixelOffsetMode = PixelOffsetMode.HighQuality;
                g.DrawImage(canvas, new Rectangle(0, 0, px, px), 0, 0, big, big, GraphicsUnit.Pixel, attrs);
            }
        }
        return result;
    }

    static byte[] Png(Bitmap b)
    {
        using (var ms = new MemoryStream()) { b.Save(ms, ImageFormat.Png); return ms.ToArray(); }
    }

    // Classic icon bitmap: header, 32-bit pixels bottom-up, then an empty 1-bit mask.
    static byte[] Dib(Bitmap b)
    {
        int s = b.Width;
        using (var ms = new MemoryStream())
        using (var w = new BinaryWriter(ms))
        {
            w.Write(40); w.Write(s); w.Write(s * 2); w.Write((short)1); w.Write((short)32);
            w.Write(0); w.Write(0); w.Write(0); w.Write(0); w.Write(0); w.Write(0);
            for (int y = s - 1; y >= 0; y--)
                for (int x = 0; x < s; x++)
                {
                    Color c = b.GetPixel(x, y);
                    w.Write(c.B); w.Write(c.G); w.Write(c.R); w.Write(c.A);
                }
            w.Write(new byte[((s + 31) / 32) * 4 * s]);
            w.Flush();
            return ms.ToArray();
        }
    }

    public static void WriteIco(string id, string path, int[] sizes)
    {
        var blobs = new List<byte[]>();
        foreach (int s in sizes)
            using (var b = Render(id, s)) blobs.Add(s >= 256 ? Png(b) : Dib(b));
        using (var w = new BinaryWriter(File.Create(path)))
        {
            w.Write((short)0); w.Write((short)1); w.Write((short)sizes.Length);
            int offset = 6 + 16 * sizes.Length;
            for (int i = 0; i < sizes.Length; i++)
            {
                byte dim = (byte)(sizes[i] >= 256 ? 0 : sizes[i]);
                w.Write(dim); w.Write(dim); w.Write((byte)0); w.Write((byte)0);
                w.Write((short)1); w.Write((short)32);
                w.Write(blobs[i].Length); w.Write(offset);
                offset += blobs[i].Length;
            }
            foreach (var blob in blobs) w.Write(blob);
        }
    }

    public static void WritePng(string id, string path, int px)
    {
        using (var b = Render(id, px)) b.Save(path, ImageFormat.Png);
    }

    // Every option at real sizes, on a dark and on a light background.
    public static void Sheet(string[] ids, string[] names, string path)
    {
        int[] sizes = { 256, 64, 48, 32, 24, 16 };
        int rowH = 364, width = 1048;
        using (var sheet = new Bitmap(width, 24 + rowH * ids.Length, PixelFormat.Format32bppArgb))
        using (var g = Graphics.FromImage(sheet))
        using (var title = new Font("Segoe UI", 15, FontStyle.Bold, GraphicsUnit.Pixel))
        using (var small = new Font("Segoe UI", 12, FontStyle.Regular, GraphicsUnit.Pixel))
        using (var ink = new SolidBrush(C("#eceff3")))
        using (var dim = new SolidBrush(C("#929bab")))
        using (var darkInk = new SolidBrush(C("#555555")))
        {
            g.Clear(C("#12151b"));
            g.TextRenderingHint = System.Drawing.Text.TextRenderingHint.ClearTypeGridFit;
            for (int r = 0; r < ids.Length; r++)
            {
                int top = 24 + r * rowH;
                g.DrawString(ids[r] + "   " + names[r], title, ink, 24, top);
                int py = top + 34, ph = 296;
                using (var dark = new SolidBrush(C("#1c1c1c"))) g.FillRectangle(dark, 24, py, 640, ph);
                using (var light = new SolidBrush(C("#f3f3f3"))) g.FillRectangle(light, 680, py, 344, ph);
                int x = 44;
                foreach (int s in sizes)
                {
                    using (var icon = Render(ids[r], s)) g.DrawImageUnscaled(icon, x, py + (ph - s) / 2);
                    g.DrawString(s.ToString(), small, dim, x, py + ph - 24);
                    x += s + 30;
                }
                x = 700;
                for (int i = 1; i < sizes.Length; i++)
                {
                    using (var icon = Render(ids[r], sizes[i])) g.DrawImageUnscaled(icon, x, py + (ph - sizes[i]) / 2);
                    g.DrawString(sizes[i].ToString(), small, darkInk, x, py + ph - 24);
                    x += sizes[i] + 30;
                }
            }
            sheet.Save(path, ImageFormat.Png);
        }
    }
}
'@

$ids = 'A', 'B', 'C', 'D'
$names = 'Field of view', 'Two rosters', 'Viewfinder', 'Letter S'
$sizes = 16, 20, 24, 32, 40, 48, 64, 256

foreach ($id in $ids) {
    [IconKit]::WritePng($id, (Join-Path $OutDir "$id-256.png"), 256)
    [IconKit]::WriteIco($id, (Join-Path $OutDir "$id.ico"), $sizes)
}
[IconKit]::Sheet($ids, $names, (Join-Path $OutDir 'sheet.png'))
"wrote options to $OutDir"

if ($Pick) {
    $final = Split-Path -Parent $OutDir
    [IconKit]::WriteIco($Pick, (Join-Path $final 'sightline.ico'), $sizes)
    [IconKit]::WritePng($Pick, (Join-Path $final 'sightline.png'), 64)
    "picked $Pick -> $final\sightline.ico, sightline.png"
}
