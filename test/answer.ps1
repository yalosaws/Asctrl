# Plays the person at the computer answering the overlay's question: presses its Allow or Deny button (through UI
# Automation) once it shows. Used by test/l5.ts.
param([ValidateSet('Allow', 'Deny')][string]$Answer = 'Deny', [int]$TimeoutSec = 20, [int]$AgentPid = 0)
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes
$root = [System.Windows.Automation.AutomationElement]::RootElement
$deadline = (Get-Date).AddSeconds($TimeoutSec)
$name = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $Answer)
$isButton = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)
$cond = New-Object System.Windows.Automation.AndCondition($name, $isButton)
while ((Get-Date) -lt $deadline) {
    $wins = $root.FindAll([System.Windows.Automation.TreeScope]::Children, [System.Windows.Automation.Condition]::TrueCondition)
    foreach ($w in $wins) {
        if ($AgentPid -and $w.Current.ProcessId -ne $AgentPid) { continue }
        $b = $w.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $cond)
        if ($b -and -not $b.Current.IsOffscreen) {
            $b.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            Write-Output "pressed $Answer"
            exit 0
        }
    }
    Start-Sleep -Milliseconds 150
}
Write-Output "no question showed"
exit 1
