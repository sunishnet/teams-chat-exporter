# Convert the project PNG into a multi-resolution Windows .ico using only the
# Windows System.Drawing assembly. The PNG remains the editable source artwork.
$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)
Add-Type -AssemblyName System.Drawing

$sourcePath = Join-Path $PSScriptRoot '..\assets\teams-chat-exporter-icon.png'
$iconPath = Join-Path $PSScriptRoot '..\assets\teams-chat-exporter.ico'
$source = [System.Drawing.Image]::FromFile([System.IO.Path]::GetFullPath($sourcePath))
$sizes = @(16, 24, 32, 48, 64, 128, 256)
$frames = @()

try {
    foreach ($size in $sizes) {
        $bitmap = [System.Drawing.Bitmap]::new($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
        $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
        $graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
        $graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
        $graphics.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
        $graphics.DrawImage($source, 0, 0, $size, $size)

        $stream = [System.IO.MemoryStream]::new()
        $bitmap.Save($stream, [System.Drawing.Imaging.ImageFormat]::Png)
        $frames += ,@{ Size = $size; Data = $stream.ToArray() }

        $stream.Dispose()
        $graphics.Dispose()
        $bitmap.Dispose()
    }
}
finally {
    $source.Dispose()
}

$file = [System.IO.File]::Open([System.IO.Path]::GetFullPath($iconPath), [System.IO.FileMode]::Create)
$writer = [System.IO.BinaryWriter]::new($file)
try {
    $writer.Write([UInt16]0)
    $writer.Write([UInt16]1)
    $writer.Write([UInt16]$frames.Count)
    $offset = 6 + (16 * $frames.Count)

    foreach ($frame in $frames) {
        $dimension = if ($frame.Size -eq 256) { [byte]0 } else { [byte]$frame.Size }
        $writer.Write($dimension)
        $writer.Write($dimension)
        $writer.Write([byte]0)
        $writer.Write([byte]0)
        $writer.Write([UInt16]1)
        $writer.Write([UInt16]32)
        $writer.Write([UInt32]$frame.Data.Length)
        $writer.Write([UInt32]$offset)
        $offset += $frame.Data.Length
    }

    foreach ($frame in $frames) { $writer.Write([byte[]]$frame.Data) }
}
finally {
    $writer.Dispose()
    $file.Dispose()
}

Write-Host "Created $iconPath with sizes: $($sizes -join ', ')"
