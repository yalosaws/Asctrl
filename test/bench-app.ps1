# A small window with known controls for test/bench.ts: ordinary controls (readable by UI Automation),
# a canvas that draws its own text (only OCR can read it), and a dialog that opens after a delay.
Add-Type -AssemblyName System.Windows.Forms
Add-Type -AssemblyName System.Drawing
[System.Windows.Forms.Application]::EnableVisualStyles()

$form = New-Object System.Windows.Forms.Form
$form.Text = 'Asctrl Bench'
$form.Size = New-Object System.Drawing.Size(720, 560)
$form.StartPosition = 'CenterScreen'
$form.TopMost = $true
$form.Font = New-Object System.Drawing.Font('Segoe UI', 11)

function Add-Control($c, $x, $y, $w, $h) {
  $c.Location = New-Object System.Drawing.Point($x, $y)
  $c.Size = New-Object System.Drawing.Size($w, $h)
  $form.Controls.Add($c)
  $c
}

$name = Add-Control (New-Object System.Windows.Forms.TextBox) 20 20 260 30
$name.AccessibleName = 'Name'
$submit = Add-Control (New-Object System.Windows.Forms.Button) 300 18 120 34
$submit.Text = 'Submit'
$result = Add-Control (New-Object System.Windows.Forms.Label) 440 24 260 28
$result.Text = 'Waiting'
$submit.Add_Click({ $result.Text = 'Submitted: ' + $name.Text })

$agree = Add-Control (New-Object System.Windows.Forms.CheckBox) 20 64 200 30
$agree.Text = 'Agree'

$notes = Add-Control (New-Object System.Windows.Forms.TextBox) 20 104 400 120
$notes.Multiline = $true
$notes.AccessibleName = 'Notes'

$openDialog = Add-Control (New-Object System.Windows.Forms.Button) 440 104 200 34
$openDialog.Text = 'Open dialog'
$openDialog.Add_Click({
  # Opens after a delay, like a real program loading something
  $t = New-Object System.Windows.Forms.Timer
  $t.Interval = 800
  $t.Add_Tick({
    $this.Stop()
    $d = New-Object System.Windows.Forms.Form
    $d.Text = 'Bench Dialog'
    $d.Size = New-Object System.Drawing.Size(320, 180)
    $d.StartPosition = 'CenterScreen'
    $d.TopMost = $true
    $d.Font = $form.Font
    $b = New-Object System.Windows.Forms.Button
    $b.Text = 'Close me'
    $b.Location = New-Object System.Drawing.Point(90, 50)
    $b.Size = New-Object System.Drawing.Size(120, 40)
    $b.Add_Click({ $this.FindForm().Close() })
    $d.Controls.Add($b)
    $d.Show()
  })
  $t.Start()
})

# A canvas: text drawn by hand, invisible to UI Automation
$script:clicks = 0
$counter = Add-Control (New-Object System.Windows.Forms.Label) 440 150 260 28
$counter.Text = 'Canvas clicks: 0'
$canvas = Add-Control (New-Object System.Windows.Forms.Panel) 20 250 660 240
$canvas.BackColor = [System.Drawing.Color]::FromArgb(30, 34, 44)
$canvas.Add_Paint({
  param($s, $e)
  $f = New-Object System.Drawing.Font('Segoe UI', 18, [System.Drawing.FontStyle]::Bold)
  $e.Graphics.DrawString('CANVAS-7391', $f, [System.Drawing.Brushes]::White, 40, 40)
  $e.Graphics.DrawString('Press here', $f, [System.Drawing.Brushes]::Gold, 380, 150)
})
$canvas.Add_MouseClick({
  param($s, $e)
  # Only clicks on the drawn words count, so a click in the wrong place is caught
  if ($e.X -ge 30 -and $e.X -le 260 -and $e.Y -ge 30 -and $e.Y -le 90) {
    $script:clicks++
    $counter.Text = "Canvas clicks: $script:clicks"
  }
})

[void]$form.ShowDialog()
