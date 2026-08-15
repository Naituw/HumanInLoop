[CmdletBinding()]
param(
  [Parameter(Mandatory = $true)]
  [string[]]$Path,

  [Parameter(Mandatory = $true)]
  [string]$ExpectedSubject
)

$ErrorActionPreference = "Stop"

foreach ($Item in $Path) {
  $Resolved = (Resolve-Path -LiteralPath $Item).Path
  $Signature = Get-AuthenticodeSignature -LiteralPath $Resolved
  if ($Signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "Authenticode validation failed for ${Resolved}: $($Signature.Status) $($Signature.StatusMessage)"
  }
  if (-not $Signature.SignerCertificate.Subject.Contains($ExpectedSubject)) {
    throw "Unexpected Authenticode subject for ${Resolved}: $($Signature.SignerCertificate.Subject)"
  }
  if ($null -eq $Signature.TimeStamperCertificate) {
    throw "The Authenticode signature is not timestamped: $Resolved"
  }
  Write-Host "Verified Authenticode signature: $Resolved"
  Write-Host "  signer:    $($Signature.SignerCertificate.Subject)"
  Write-Host "  issuer:    $($Signature.SignerCertificate.Issuer)"
  Write-Host "  timestamp: $($Signature.TimeStamperCertificate.Subject)"
}
