using System.Collections.ObjectModel;
using System.ComponentModel;
using System.Windows;
using System.Windows.Threading;
using Microsoft.Win32;
using SwappableVRAM.Core;

namespace SwappableVRAM.App;

public partial class MainWindow : Window
{
    private readonly DispatcherTimer timer = new() { Interval = TimeSpan.FromSeconds(2) };
    private readonly ObservableCollection<ResultRow> results = [];
    private readonly SessionRecorder recorder = new();
    private GpuTelemetry? gpu;
    private GpuDevice? device;
    private BenchmarkReport? report;
    private CancellationTokenSource? cancellation;
    private bool closingAfterRun;
    private bool checkingBroker;

    public MainWindow()
    {
        InitializeComponent();
        ResultsGrid.ItemsSource = results;
        DiskDirectory.Text = System.IO.Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "SwappableVRAM", "LabIO");
        timer.Tick += (_, _) => RefreshTelemetry();
    }
    private async void Window_Loaded(object sender, RoutedEventArgs e)
    {
        try
        {
            var local = await Task.Run(LocalBackendStatus.Capture);
            LocalBackendDetail.Text = local.DriverInterfaces == 0
                ? "Windows bellek sürücüsü henüz bu bilgisayarda kurulu değil. GPU testleri yerel ekran kartında çalışır."
                : "Yerel bellek sürücüsü bulundu; sistem offload özelliğinin doğrulaması henüz tamamlanmadı.";
        }
        catch (Exception ex) { LocalBackendDetail.Text = $"Yerel bellek bağlantısı okunamadı: {ex.Message}"; }
        try
        {
            device = (await Task.Run(GpuCatalog.Enumerate)).FirstOrDefault();
            if (device is null) throw new InvalidOperationException("CUDA destekli NVIDIA GPU bulunamadı.");
            gpu = new(device.PciBusId);
        }
        catch (Exception ex)
        {
            RunStatus.Text = Friendly(ex);
            RunButton.IsEnabled = false;
        }
        RefreshTelemetry();
        timer.Start();
    }
    private void RefreshTelemetry()
    {
        try
        {
            var memory = SystemSnapshot.Capture();
            var snapshot = gpu?.Capture();
            var processes = ProcessTelemetry.Capture();
            RamValue.Text = GiB(memory.PhysicalAvailable);
            RamDetail.Text = $"{GiB(memory.PhysicalTotal)} toplam · %{100 * (memory.PhysicalTotal - memory.PhysicalAvailable) / memory.PhysicalTotal} kullanım";
            CommitValue.Text = $"{GiB(memory.CommitTotal)} / {GiB(memory.CommitLimit)}";
            if (snapshot is { Error: null })
            {
                GpuValue.Text = GiB(snapshot.FreeBytes);
                GpuDetail.Text = $"{device?.Name} · GPU %{snapshot.Utilization?.ToString() ?? "?"}";
            }
            else { GpuValue.Text = "Bilinmiyor"; GpuDetail.Text = snapshot?.Error ?? "NVML telemetrisi yok"; }
            ProcessGrid.ItemsSource = processes.GroupBy(p => p.Name, StringComparer.OrdinalIgnoreCase)
                .Select(g => new ProcessRow(g.Key, g.Count(), MiB(g.Sum(p => p.WorkingSetBytes)), MiB(g.Sum(p => p.PrivateBytes)))).ToArray();
            GameStatus.Text = processes.Any(p => p.Name.Equals("cs2", StringComparison.OrdinalIgnoreCase)) ? "● CS2 açık · bellek ayarlarına müdahale edilmiyor" : "CS2 şu anda çalışmıyor";
            recorder.Add(new(memory, snapshot, processes));
            if (recorder.Recording) RecordStatus.Text = $"Kayıt sürüyor · {recorder.Count} örnek · sistem offload kapalı";
        }
        catch (Exception ex) { Footer.Text = $"Telemetri: {ex.Message}"; }
    }
    private async void BrokerCheck_Click(object sender, RoutedEventArgs e)
    {
        if (checkingBroker) return;
        checkingBroker = true;
        BrokerCheckButton.IsEnabled = false;
        LocalBackendDetail.Text = "Yerel Rust broker bağlantısı denetleniyor…";
        try
        {
            await using var broker = await BrokerSession.ConnectAsync(System.IO.Path.Combine(AppContext.BaseDirectory, "SwappableVRAM.Broker.exe"));
            var status = await broker.QueryAsync();
            await broker.StopAsync();
            LocalBackendDetail.Text = status.State == BrokerState.NoDriver
                ? "Rust broker bağlantısı doğrulandı ve kapatıldı. Windows bellek sürücüsü kurulu değil; RAM aktarımı başlamadı."
                : $"Rust broker bağlantısı doğrulandı ve kapatıldı. {status.DriverCount} sürücü bulundu; bellek katmanı henüz kullanıma hazır değil.";
        }
        catch (Exception ex) { LocalBackendDetail.Text = $"Yerel bağlantı tamamlanamadı: {ex.Message}"; }
        finally
        {
            checkingBroker = false;
            BrokerCheckButton.IsEnabled = true;
            if (closingAfterRun && cancellation is null) Close();
        }
    }
    private async void Run_Click(object sender, RoutedEventArgs e)
    {
        if (device is null || cancellation is not null) return;
        if (!int.TryParse(PoolInput.Text, out var pool) || pool is < 16 or > 1024 ||
            !int.TryParse(SamplesInput.Text, out var samples) || samples is < 100 or > 100_000 ||
            !int.TryParse(GapInput.Text, out var gap) || gap is < 0 or > 100)
        {
            RunStatus.Text = "VRAM: 16–1024 MiB; örnek: 100–100000; aralık: 0–100 ms olmalı.";
            return;
        }
        if (DiskCheck.IsChecked == true && string.IsNullOrWhiteSpace(DiskDirectory.Text)) { RunStatus.Text = "Disk karşılaştırması için bir klasör gir."; return; }
        var options = new BenchmarkOptions(device.Ordinal, pool, samples, gap, DiskCheck.IsChecked == true ? DiskDirectory.Text : null);
        cancellation = new();
        var token = cancellation.Token;
        SetRunning(true);
        report = null;
        results.Clear();
        RunProgress.Value = 0;
        var progress = new Progress<BenchmarkProgress>(p =>
        {
            RunStatus.Text = p.Stage;
            RunProgress.Value = p.Percent;
            if (p.Result is { } value) results.Add(new(value));
        });
        try
        {
            report = await Task.Run(() => LatencyBenchmark.Run(options, progress, token), token);
            RunStatus.Text = "Tamamlandı · bütünlük doğrulandı · VRAM bırakıldı. Bu sonuç FPS kazanımı göstermez.";
        }
        catch (OperationCanceledException) { RunStatus.Text = "İptal edildi · test kaynakları bırakıldı. Kısmi sonuçlar geçerli rapor değildir."; results.Clear(); }
        catch (Exception ex) { RunStatus.Text = Friendly(ex); results.Clear(); }
        finally
        {
            cancellation.Dispose();
            cancellation = null;
            SetRunning(false);
            if (closingAfterRun) Close();
        }
    }
    private void SetRunning(bool value)
    {
        RunButton.IsEnabled = !value && device is not null;
        CancelButton.IsEnabled = value;
        ExportButton.IsEnabled = !value && report is not null;
        PoolInput.IsEnabled = SamplesInput.IsEnabled = GapInput.IsEnabled = DiskCheck.IsEnabled = !value;
    }
    private void Cancel_Click(object sender, RoutedEventArgs e) => cancellation?.Cancel();
    private void Export_Click(object sender, RoutedEventArgs e)
    {
        if (report is null) return;
        var dialog = new SaveFileDialog { Filter = "JSON raporu (*.json)|*.json", FileName = $"swappablevram-latency-{DateTime.Now:yyyyMMdd-HHmmss}.json" };
        if (dialog.ShowDialog(this) == true)
        {
            try { report.Save(dialog.FileName); RunStatus.Text = $"Rapor kaydedildi: {dialog.FileName}"; }
            catch (Exception ex) { RunStatus.Text = ex.Message; }
        }
    }
    private void Record_Click(object sender, RoutedEventArgs e)
    {
        if (!recorder.Recording)
        {
            recorder.Start();
            RecordButton.Content = "Durdur ve kaydet";
            RefreshTelemetry();
            return;
        }
        recorder.Stop();
        RecordButton.Content = "Kaydı başlat";
        SaveSession();
    }
    private void SaveSession()
    {
        var dialog = new SaveFileDialog { Filter = "JSON oturumu (*.json)|*.json", FileName = $"swappablevram-baseline-{DateTime.Now:yyyyMMdd-HHmmss}.json" };
        if (dialog.ShowDialog(this) != true) { RecordStatus.Text = $"Kayıt durdu · {recorder.Count} örnek henüz kaydedilmedi"; return; }
        try { recorder.Save(dialog.FileName); RecordStatus.Text = $"{recorder.Count} örnek kaydedildi"; }
        catch (Exception ex) { RecordStatus.Text = ex.Message; }
    }
    private void Window_Closing(object? sender, CancelEventArgs e)
    {
        if (cancellation is not null || checkingBroker)
        {
            e.Cancel = true;
            closingAfterRun = true;
            cancellation?.Cancel();
            RunStatus.Text = "Yerel işlemler kapatılıyor…";
            return;
        }
        timer.Stop();
        gpu?.Dispose();
    }
    private static string Friendly(Exception ex) => ex is DllNotFoundException
        ? "Rust motoru yüklenemedi. Uygulamayı DLL'leriyle aynı yayın klasöründen çalıştır."
        : $"Ölçüm tamamlanmadı: {ex.Message}";
    private static string GiB(long bytes) => $"{bytes / (1024d * 1024 * 1024):N1} GiB";
    private static string MiB(long bytes) => $"{bytes / (1024d * 1024):N0} MiB";
    private sealed record ProcessRow(string Name, int Count, string WorkingSet, string Private);
    private sealed record ResultRow(string Label, string P50, string P95, string P99, string Max)
    {
        public ResultRow(LatencyDistribution d) : this($"{d.Path} · {d.Operation}", $"{d.P50Us:N1}", $"{d.P95Us:N1}", $"{d.P99Us:N1}", $"{d.MaxUs:N1}") { }
    }
}
