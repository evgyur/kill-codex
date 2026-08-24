using System.Diagnostics;
using System.Drawing;

namespace CodexProcessKiller;

internal static class Program
{
    [STAThread]
    private static void Main(string[] args)
    {
        if (args.Contains("--self-test", StringComparer.OrdinalIgnoreCase))
        {
            var cases = new Dictionary<string, bool>(StringComparer.OrdinalIgnoreCase)
            {
                ["ChatGPT"] = true,
                ["codex"] = true,
                ["codex-code-mode-host"] = true,
                ["codex-computer-use"] = true,
                ["CodexProcessKiller"] = false,
                ["explorer"] = false
            };

            Environment.ExitCode = cases.All(item => ProcessKiller.IsTarget(item.Key) == item.Value) ? 0 : 1;
            return;
        }

        if (args.Contains("--dry-run", StringComparer.OrdinalIgnoreCase))
        {
            var targets = ProcessKiller.FindTargets().ToArray();
            var lines = targets.Select(process => $"{process.ProcessName} (PID {process.Id})").ToArray();
            foreach (var process in targets)
            {
                process.Dispose();
            }

            var message = lines.Length == 0
                ? "Процессы Codex не найдены."
                : "Будут завершены:\n\n" + string.Join("\n", lines);
            MessageBox.Show(message, "Kill Codex — безопасная проверка", MessageBoxButtons.OK, MessageBoxIcon.Information);
            return;
        }

        ApplicationConfiguration.Initialize();
        Application.Run(new KillerForm());
    }
}

internal static class ProcessKiller
{
    internal static bool IsTarget(string processName) =>
        processName.Equals("ChatGPT", StringComparison.OrdinalIgnoreCase) ||
        processName.Equals("codex", StringComparison.OrdinalIgnoreCase) ||
        processName.StartsWith("codex-", StringComparison.OrdinalIgnoreCase);

    internal static IEnumerable<Process> FindTargets() =>
        Process.GetProcesses()
            .Where(process => IsTarget(process.ProcessName) && process.Id != Environment.ProcessId)
            .OrderBy(process => process.ProcessName)
            .ThenBy(process => process.Id);

    internal static KillResult KillAll()
    {
        var killed = new List<string>();
        var failed = new List<string>();

        foreach (var process in FindTargets())
        {
            using (process)
            {
                var label = $"{process.ProcessName} (PID {process.Id})";
                try
                {
                    process.Kill(entireProcessTree: true);
                    killed.Add(label);
                }
                catch (Exception ex) when (ex is InvalidOperationException or System.ComponentModel.Win32Exception or NotSupportedException)
                {
                    failed.Add($"{label}: {ex.Message}");
                }
            }
        }

        return new KillResult(killed, failed);
    }
}

internal sealed record KillResult(IReadOnlyList<string> Killed, IReadOnlyList<string> Failed);

internal sealed class KillerForm : Form
{
    private readonly Button _killButton;
    private readonly Label _status;
    private readonly TextBox _details;

    internal KillerForm()
    {
        Text = "Убить процессы Codex";
        ClientSize = new Size(520, 360);
        MinimumSize = new Size(480, 320);
        StartPosition = FormStartPosition.CenterScreen;
        BackColor = Color.FromArgb(246, 247, 249);
        Font = new Font("Segoe UI", 10F);

        var title = new Label
        {
            Text = "Codex Process Killer",
            Font = new Font("Segoe UI Semibold", 20F),
            AutoSize = true,
            Location = new Point(28, 24)
        };

        var subtitle = new Label
        {
            Text = "Завершает ChatGPT.exe, codex.exe и все процессы codex-*.",
            AutoSize = true,
            ForeColor = Color.FromArgb(80, 86, 96),
            Location = new Point(31, 68)
        };

        _killButton = new Button
        {
            Text = "УБИТЬ ВСЕ ПРОЦЕССЫ CODEX",
            Font = new Font("Segoe UI Semibold", 12F),
            BackColor = Color.FromArgb(196, 44, 44),
            ForeColor = Color.White,
            FlatStyle = FlatStyle.Flat,
            Location = new Point(32, 106),
            Size = new Size(454, 58),
            Anchor = AnchorStyles.Top | AnchorStyles.Left | AnchorStyles.Right
        };
        _killButton.FlatAppearance.BorderSize = 0;
        _killButton.Click += KillButton_Click;

        _status = new Label
        {
            Text = "Готово к запуску",
            Font = new Font("Segoe UI Semibold", 10F),
            AutoSize = true,
            Location = new Point(31, 184)
        };

        _details = new TextBox
        {
            ReadOnly = true,
            Multiline = true,
            ScrollBars = ScrollBars.Vertical,
            BackColor = Color.White,
            BorderStyle = BorderStyle.FixedSingle,
            Location = new Point(32, 212),
            Size = new Size(454, 112),
            Anchor = AnchorStyles.Top | AnchorStyles.Bottom | AnchorStyles.Left | AnchorStyles.Right,
            Text = "После нажатия здесь появится результат."
        };

        Controls.AddRange([title, subtitle, _killButton, _status, _details]);
        AcceptButton = _killButton;
    }

    private void KillButton_Click(object? sender, EventArgs e)
    {
        _killButton.Enabled = false;
        var result = ProcessKiller.KillAll();

        _status.Text = result.Failed.Count == 0
            ? $"Готово: завершено процессов — {result.Killed.Count}"
            : $"Завершено: {result.Killed.Count}; ошибок: {result.Failed.Count}";

        var lines = result.Killed.Select(item => $"✓ {item}")
            .Concat(result.Failed.Select(item => $"✗ {item}"))
            .ToArray();
        _details.Lines = lines.Length == 0 ? ["Процессы Codex не найдены."] : lines;
        _killButton.Enabled = true;
    }
}
