Add-Content -Path "$env:TEMP\foo.txt" -Value "foo"
Get-Content "$env:TEMP\foo.txt"
