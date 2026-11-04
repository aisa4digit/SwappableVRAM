# Aynı Windows oturumunda Rust broker

RTX 5060 için CUDA worker bir Windows LocalSystem servisi olarak tasarlanmamalı. NVIDIA'nın CUDA 11.5.1 Windows kılavuzu servislerde CUDA kullanımını TCC'ye bağlıyor ve WDDM aygıtlarda bunun mümkün olmadığını belirtiyor. Güncel kılavuz da GeForce kartlarının GTX Titan istisnası dışında TCC desteklemediğini söylüyor. Bu nedenle Session 0 CUDA worker'ı seçmek bu makine için belgelenmiş bir üretim yolu değildir. [NVIDIA servis/TCC açıklaması](https://docs.nvidia.com/cuda/archive/11.5.1/cuda-installation-guide-microsoft-windows/index.html), [güncel Windows CUDA kılavuzu](https://docs.nvidia.com/cuda/cuda-installation-guide-microsoft-windows/index.html).

GUI artık ayrı Rust yardımcı sürecini başlatıp gerçek yerel named pipe üzerinden durumunu sorgulayabilir ve kapatabilir. **Yerel bağlantıyı denetle** bu akışı kullanır. Açılışta helper veya UAC başlatılmaz. Varsayılan helper CUDA tahsis etmez, kernel cache etkinleştirmez; START komutuna hazır olmayan durumu döndürür. Bu bağlantı henüz uygulamaların RAM sayfalarını VRAM'e taşıdığı anlamına gelmez.

CUDA ve DXGI budget monitor aynı broker PID'sinde kalmalıdır. QueryVideoMemoryInfo o sürecin budget ve CurrentUsage değerlerini verir; oyunun PID'sinin bütçesini veya bütün GPU'nun boş VRAM'ini bildirmez. Oyun headroom'u ayrı global bellek gözlemiyle korunur. [Microsoft DXGI belgesi](https://learn.microsoft.com/en-us/windows/win32/api/dxgi1_4/nf-dxgi1_4-idxgiadapter3-queryvideomemoryinfo).

## Kontrol bağlantısı

- GUI, rastgele adlı pipe'ı helper'dan önce `FILE_FLAG_FIRST_PIPE_INSTANCE`, message/overlapped ve `PIPE_REJECT_REMOTE_CLIENTS` ile oluşturur. ACL yalnız GUI kullanıcısının TokenUser SID'sine okuma/yazma verir. `CurrentUserOnly` kullanılmaz: yükseltilmiş split token'ın TokenOwner kimliği farklı olabilir.
- GUI pipe client PID'sini başlattığı Process nesnesinin PID'siyle ve aynı interaktif oturumla karşılaştırır. Rust helper server PID'sini GUI PID'siyle, iki sürecin TokenUser SID'sini ve oturumunu karşılaştırır; açık process handle ile ebeveynin yaşadığını izler. Komut satırındaki nonce tek başına kimlik doğrulama sayılmaz.
- Protokol sabit 128 byte ve sürüm 1'dir. Hello/status/start/stop/query dışında komut, yol, disk verisi veya keyfi IOCTL taşımaz. PID/oturum/32-byte nonce her mesajda; istek sırası artan ve sıfırdan farklıdır. Kısa/uzun mesaj, reserved alan, yanlış kimlik ve tekrar edilen komut reddedilir.
- .NET komutları seri hale getirir; tek reader yanıtları sıra numarasıyla eşler. Zaman aşımı veya protokol hatasında bütün oturum kapatılır. Rust her overlapped read/write iptalini tamamlanana kadar drain eder; pending buffer serbest bırakılmaz.
- STOP cevabı worker bittikten sonra gönderilir. GUI/pipe kaybı kendi worker'ına stop verir; kaynak bırakma takılırsa yalnız yardımcı süreç sonlanır. .NET de süre aşımında yalnız kendi oluşturduğu Process nesnesini kapatır. Kernel'in bağımsız deadline/SSD fallback yolu bundan ayrıdır.

`broker-host <pipe> <parent-pid> <nonce> --experimental` açıkça seçilen geliştirme profilidir. Yükseltilmiş aynı kullanıcı/oturum, runtime test-signing, WDDM desteği, tek SVRAMLAB PnP hedefi, boot-local ID ve idle/D0/uyumlu ABI gerektirir. Güncel GUI bu profili başlatmaz veya UAC istemez. Bu profil ve yeni aralık protokolü canlı host kernel üzerinde henüz doğrulanmadı; hostta filtre arayüzü yoktur. Microsoft imzası teslim şartı kaldırılmıştır; yüklenebilir kişisel test paketi ve gerçek paging doğrulaması halen gerekir.

Status ve boot-local ID aynı açık kontrol bağlantısından alınır. Worker, GUI'nin seçtiği ID'yi kendi açtığı bağlantıda cache etkinleştirilmeden ve GPU tahsisinden önce yeniden doğrular. Kaldırılıp tekrar eklenen aynı PnP yolu sessizce yeni hedefe bağlanmaz. Yükseltilmiş dağıtım yolunda helper fiziksel, korunan kurulum konumunda olmalıdır; normal kullanıcıya ait mapped `V:` sürücüsünün yükseltilmiş oturumda bulunması garanti edilmez. Mevcut probe yükseltilmez.

## Yerel doğrulama

```powershell
powershell -NoProfile -File scripts/Build-Desktop.ps1
dotnet run --project src/SwappableVRAM.Diagnostics -c Release -- controller-test artifacts/desktop/SwappableVRAM.Broker.exe
```

Test normal yetkili gerçek .NET/Rust süreçlerinde handshake, durum, varsayılan START reddi, 16 eşzamanlı sorgunun seri yanıtlanması, STOP, bağlantı kaybı ve çocuğun tamamen çıkışını denetler. Yanlış nonce/PID, tekrar edilen komut ve 129-byte mesaj ayrı gerçek çocuk süreçlerde reddedilir. GPU tahsisi veya pagefile testi değildir. Yayın klasöründe self-contained WPF uygulaması ve WDDM özellikli `SwappableVRAM.Broker.exe` bulunur; derleme uygulamayı otomatik açmaz.
