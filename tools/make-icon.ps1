# Generates ui/fern.ico.
#
# Windows only, and not part of the build: it just needs the System.Drawing
# assembly that ships with .NET, so no package manager or image tool is
# involved. The shape lives in the drawing code below; ui/tray-icon.svg is the
# hand-drawn reference of the same design, so update both when you change it.
#
#   powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
#   pwsh      -File tools\make-icon.ps1
#
# Small sizes stay flat (a 16 px tile cannot show any more detail); the two
# label rules are drawn from 32 px up. Every entry below 256 px is a plain
# 32bpp DIB, 256 px is PNG-compressed, which is what Explorer expects.

param(
    [string]$Output = (Join-Path (Split-Path -Parent $PSScriptRoot) 'ui\fern.ico')
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

$Green = [System.Drawing.Color]::FromArgb(255, 0x20, 0xC9, 0x97)
$Blue = [System.Drawing.Color]::FromArgb(255, 0x2C, 0x6D, 0xD2)
$Paper = [System.Drawing.Color]::FromArgb(255, 0xF4, 0xFD, 0xFA)

function New-RoundedPath {
    param([single]$X, [single]$Y, [single]$W, [single]$H, [single]$R)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [single]($R * 2)
    $path.AddArc($X, $Y, $d, $d, [single]180, [single]90)
    $path.AddArc([single]($X + $W - $d), $Y, $d, $d, [single]270, [single]90)
    $path.AddArc([single]($X + $W - $d), [single]($Y + $H - $d), $d, $d, [single]0, [single]90)
    $path.AddArc($X, [single]($Y + $H - $d), $d, $d, [single]90, [single]90)
    $path.CloseFigure()
    return $path
}

# Shutter corner: square on the inner sides, rounded on the outer corner that
# it shares with the case.
function New-ShutterPath {
    param([single]$X1, [single]$Y1, [single]$X2, [single]$Y2, [single]$R)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [single]($R * 2)
    $path.AddLine($X1, $Y1, [single]($X2 - $R), $Y1)
    $path.AddArc([single]($X2 - $d), $Y1, $d, $d, [single]270, [single]90)
    $path.AddLine($X2, [single]($Y1 + $R), $X2, $Y2)
    $path.AddLine($X2, $Y2, $X1, $Y2)
    $path.CloseFigure()
    return $path
}

function New-IconBitmap {
    param([int]$Size)

    $bmp = New-Object System.Drawing.Bitmap($Size, $Size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    try {
        $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
        $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
        $g.Clear([System.Drawing.Color]::Transparent)

        $s = [single]($Size / 16.0)
        $green = New-Object System.Drawing.SolidBrush($Green)
        $blue = New-Object System.Drawing.SolidBrush($Blue)
        $paper = New-Object System.Drawing.SolidBrush($Paper)
        $body = New-RoundedPath ([single](1 * $s)) ([single](1 * $s)) ([single](14 * $s)) ([single](14 * $s)) ([single](2 * $s))
        $shutter = New-ShutterPath ([single](9 * $s)) ([single](1 * $s)) ([single](15 * $s)) ([single](6 * $s)) ([single](1.5 * $s))
        $label = New-RoundedPath ([single](3.5 * $s)) ([single](8.5 * $s)) ([single](9 * $s)) ([single](5 * $s)) ([single](0.75 * $s))
        try {
            $g.FillPath($green, $body)
            $g.FillPath($blue, $shutter)
            $g.FillPath($paper, $label)

            if ($Size -ge 32) {
                $rule1 = New-RoundedPath ([single](5 * $s)) ([single](9.8 * $s)) ([single](5.8 * $s)) ([single](1 * $s)) ([single](0.5 * $s))
                $rule2 = New-RoundedPath ([single](5 * $s)) ([single](11.4 * $s)) ([single](4.2 * $s)) ([single](1 * $s)) ([single](0.5 * $s))
                try {
                    $g.FillPath($blue, $rule1)
                    $g.FillPath($blue, $rule2)
                } finally {
                    $rule1.Dispose()
                    $rule2.Dispose()
                }
            }
        } finally {
            $body.Dispose()
            $shutter.Dispose()
            $label.Dispose()
            $green.Dispose()
            $blue.Dispose()
            $paper.Dispose()
        }
    } finally {
        $g.Dispose()
    }
    return $bmp
}

function Get-DibEntry {
    param([System.Drawing.Bitmap]$Bitmap)

    $w = $Bitmap.Width
    $h = $Bitmap.Height
    $rect = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
    $locked = $Bitmap.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    try {
        $stride = $locked.Stride
        $pixels = New-Object byte[] ($stride * $h)
        [System.Runtime.InteropServices.Marshal]::Copy($locked.Scan0, $pixels, 0, $pixels.Length)
    } finally {
        $Bitmap.UnlockBits($locked)
    }

    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    try {
        $bw.Write([BitConverter]::GetBytes([int]40))       # biSize
        $bw.Write([BitConverter]::GetBytes([int]$w))       # biWidth
        $bw.Write([BitConverter]::GetBytes([int]($h * 2))) # biHeight: colour rows + AND mask
        $bw.Write([BitConverter]::GetBytes([int16]1))      # biPlanes
        $bw.Write([BitConverter]::GetBytes([int16]32))     # biBitCount
        $bw.Write([BitConverter]::GetBytes([int]0))        # biCompression = BI_RGB
        $bw.Write([BitConverter]::GetBytes([int]($w * $h * 4)))
        $bw.Write([BitConverter]::GetBytes([int]0))        # biXPelsPerMeter
        $bw.Write([BitConverter]::GetBytes([int]0))        # biYPelsPerMeter
        $bw.Write([BitConverter]::GetBytes([int]0))        # biClrUsed
        $bw.Write([BitConverter]::GetBytes([int]0))        # biClrImportant

        $rowBytes = $w * 4
        $row = New-Object byte[] $rowBytes
        for ($y = $h - 1; $y -ge 0; $y--) {
            [Array]::Copy($pixels, $y * $stride, $row, 0, $rowBytes)
            $bw.Write($row)
        }

        # Alpha carries the shape, so the 1bpp mask stays empty.
        $zero = New-Object byte[] ((([int](($w + 31) / 32)) * 4))
        for ($y = 0; $y -lt $h; $y++) { $bw.Write($zero) }
        $bw.Flush()
        return $ms.ToArray()
    } finally {
        $bw.Dispose()
        $ms.Dispose()
    }
}

function Get-PngEntry {
    param([System.Drawing.Bitmap]$Bitmap)
    $ms = New-Object System.IO.MemoryStream
    try {
        $Bitmap.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        return $ms.ToArray()
    } finally {
        $ms.Dispose()
    }
}

$sizes = @(16, 20, 24, 32, 48, 256)
$images = @()
foreach ($size in $sizes) {
    $bmp = New-IconBitmap -Size $size
    try {
        if ($size -ge 256) { $images += ,(Get-PngEntry -Bitmap $bmp) }
        else { $images += ,(Get-DibEntry -Bitmap $bmp) }
    } finally {
        $bmp.Dispose()
    }
}

$full = [System.IO.Path]::GetFullPath($Output)
$fs = New-Object System.IO.FileStream($full, [System.IO.FileMode]::Create, [System.IO.FileAccess]::Write)
$bw = New-Object System.IO.BinaryWriter($fs)
try {
    $bw.Write([BitConverter]::GetBytes([int16]0))  # reserved
    $bw.Write([BitConverter]::GetBytes([int16]1))  # type = icon
    $bw.Write([BitConverter]::GetBytes([int16]$sizes.Count))

    $offset = 6 + 16 * $sizes.Count
    for ($i = 0; $i -lt $sizes.Count; $i++) {
        $side = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
        $bw.Write([byte]$side)
        $bw.Write([byte]$side)
        $bw.Write([byte]0)                                  # palette entries
        $bw.Write([byte]0)                                  # reserved
        $bw.Write([BitConverter]::GetBytes([int16]1))       # colour planes
        $bw.Write([BitConverter]::GetBytes([int16]32))      # bits per pixel
        $bw.Write([BitConverter]::GetBytes([int]$images[$i].Length))
        $bw.Write([BitConverter]::GetBytes([int]$offset))
        $offset += $images[$i].Length
    }
    foreach ($image in $images) { $bw.Write([byte[]]$image) }
    $bw.Flush()
} finally {
    $bw.Dispose()
    $fs.Dispose()
}

$item = Get-Item -LiteralPath $full
Write-Host ("wrote {0} ({1} bytes, sizes {2})" -f $item.FullName, $item.Length, ($sizes -join '/'))
