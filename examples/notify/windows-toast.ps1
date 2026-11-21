# Generic Windows desktop toast for a kelir [notify] hook (M11).
# In config.toml (with shell = ["powershell", "-NoProfile", "-Command"]):
#   [notify]
#   turn_done = "& '/path/to/windows-toast.ps1'"
# The hook's environment is scrubbed; the event and message arrive in
# $env:KELIR_NOTIFY_EVENT and $env:KELIR_NOTIFY_MESSAGE. No keys or tokens.
$event = if ($env:KELIR_NOTIFY_EVENT) { $env:KELIR_NOTIFY_EVENT } else { 'kelir' }
$message = if ($env:KELIR_NOTIFY_MESSAGE) { $env:KELIR_NOTIFY_MESSAGE } else { 'work ended' }
# Fold control bytes so a message cannot inject another terminal sequence.
$message = ($message -replace '[\x00-\x1F]', ' ')
try {
    [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null
    $template = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent(
        [Windows.UI.Notifications.ToastTemplateType]::ToastText02)
    $text = $template.GetElementsByTagName('text')
    $text.Item(0).AppendChild($template.CreateTextNode("kelir: $event")) | Out-Null
    $text.Item(1).AppendChild($template.CreateTextNode($message)) | Out-Null
    $toast = [Windows.UI.Notifications.ToastNotification]::new($template)
    [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('kelir').Show($toast)
} catch {
    # A toast is a courtesy: a failure never changes the session's outcome.
    Write-Output "kelir: toast failed: $_"
}
