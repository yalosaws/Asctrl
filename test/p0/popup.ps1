# An unexpected popup, for the P0 benchmark's --popups switch (docs/architecture-v2.md section 7): a small window
# that comes up in front of the work in the middle of a task, like an update notice, and waits to be dismissed.
# "Later" or the close box dismiss it; it goes by itself after -Seconds.
param([int]$Seconds = 120)
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object System.Windows.Forms.Form
$f.Text = 'Update available'
$f.Size = New-Object System.Drawing.Size(380, 170)
$f.StartPosition = 'CenterScreen'
$f.TopMost = $true
$f.FormBorderStyle = 'FixedDialog'
$f.MaximizeBox = $false
$f.MinimizeBox = $false
$l = New-Object System.Windows.Forms.Label
$l.Text = 'A new version of Contoso Helper is ready to install.'
$l.Location = New-Object System.Drawing.Point(16, 18)
$l.Size = New-Object System.Drawing.Size(340, 40)
$f.Controls.Add($l)
$later = New-Object System.Windows.Forms.Button
$later.Text = 'Later'
$later.Location = New-Object System.Drawing.Point(160, 80)
$later.Size = New-Object System.Drawing.Size(90, 30)
$later.Add_Click({ $f.Close() })
$f.Controls.Add($later)
$install = New-Object System.Windows.Forms.Button
$install.Text = 'Install now'
$install.Location = New-Object System.Drawing.Point(262, 80)
$install.Size = New-Object System.Drawing.Size(90, 30)
$install.Add_Click({ $f.Close() })
$f.Controls.Add($install)
$t = New-Object System.Windows.Forms.Timer
$t.Interval = $Seconds * 1000
$t.Add_Tick({ $f.Close() })
$t.Start()
[void]$f.ShowDialog()
