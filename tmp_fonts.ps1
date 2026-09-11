$path = 'd:\code\rust\water_mark\tmp_docx\word\document.xml'
$t = [IO.File]::ReadAllText($path, [Text.Encoding]::UTF8)
Write-Output "document.xml 字符数: $($t.Length)"
Write-Output "--- 声明的字体 (按出现次数) ---"
[regex]::Matches($t, 'w:(?:ascii|eastAsia|hAnsi|cs|val)="([^"]+)"') |
    ForEach-Object { $_.Groups[1].Value } |
    Where-Object { $_ -notmatch '^[0-9]+$' } |
    Group-Object |
    Sort-Object Count -Descending |
    Select-Object -First 25 |
    ForEach-Object { "{0,6}  {1}" -f $_.Count, $_.Name }
