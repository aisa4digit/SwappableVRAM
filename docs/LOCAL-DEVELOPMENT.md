# Yerel Windows geliştirme durumu

VRAM motoru gerçek Windows/NVIDIA donanımı üzerinde çalışır. İzole kernel fixture sonuçları bu bilgisayarda uygulama belleğinin VRAM'e taşındığı anlamına gelmez.

2026-11-04 yerel kontrol: Windows 11 Pro 28000, RTX 5060, yaklaşık 16 GiB RAM; çalışma oturumu yükseltilmiş yönetici değildir. Runtime code-integrity sorgusunda test-signing kapalıdır ve SwappableVRAM PnP arayüzü bulunmamıştır. Test paketi geçici laboratuvar sertifikası taşır; üretim için güvenilir sürücü değildir. Bu kontroller host ayarlarını değiştirmedi.

Yerel ölçümler:

| Kontrol | Sonuç | Ham çıktı |
| --- | --- | --- |
| Gerçek GPU cache ve SSD asıl kopya | 2.048 hit, VRAM bırakıldıktan sonra 2.048 disk okuması; veri bütünlüğü geçti | `artifacts/measurements/local-cache-integrity-20261104-0310.log` |
| Ayrı süreç CUDA, 4 KiB QD1 | p50 68,8 µs, p99 122,2 µs; payload/generation/timeout/geç yanıt testleri geçti | `artifacts/measurements/local-ipc-20261104-0310.json` |
| Rust/.NET yerel durum köprüsü | DriverInterfaces=0, TestSigning=false | `artifacts/measurements/local-dotnet-status-20261104-0312.log` |

Bu ölçümlerde CS2 ve belirtilen arka plan uygulamaları açıktı; aktif maç veya tekrarlanabilir oyun sahnesi doğrulanmadı. Değerler kernel page-fault gecikmesi, net RAM kazancı veya FPS sonucu olarak kullanılmaz.

`svram host-status` salt okunur kontrol yapar. Masaüstü arayüzü aynı Rust durum API'siyle yerel sürücü bağlantısını gösterir. Son yayın `artifacts/desktop` altındadır; derlendi, ekranı ele almamak için otomatik açılmadı. Sistem offload özelliği kapalıdır.

Yerel geliştirme sırasında uygulama belleklerini topluca trim etmek, host pagefile'ını değiştirmek, test-signing açmak veya doğrulanmamış storage filtresini fiziksel diske kurmak mevcut uygulamanın işlevi değildir. Genel uygulama sayfalarının VRAM katmanını kullanması için Windows'un yükleyebileceği, veri bütünlüğü ve kaynak maliyeti doğrulanmış bir backend tamamlanmalıdır. GPU motorunun çalışması bu engeli tek başına çözmez.

## Güvenilir imza teslim şartı değil

2026-11-05 kullanıcı yönlendirmesiyle Microsoft/WHQL/attestation imzası hedef kapsamından çıkarıldı. Yerel geliştirme/test paketi üzerinde çalışmaya devam edilir. Backend'in teknik doğrulaması (gerçek paging, coherence, timeout, net RAM ve frametime) imzadan bağımsız gerekliliktir.

Meşru kişisel geliştirme yolu kendi test sertifikamızla SYS/CAT test imzası ve Windows test-signing modudur. Bu, otomatik uygulanacak bir ayar değildir: Secure Boot test-signing değişikliğini engelleyebilir; boot değişikliği ve yeniden başlatma gerekir. HVCI açıkken de tamamen imzasız binary desteklenmez; test imzalı binary'nin ayrıca HVCI ile uyumlu olması gerekir. Proje bu ayarları değiştirmedi, ana makineye sertifika veya sürücü kurmadı. [Microsoft TESTSIGNING](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/the-testsigning-boot-configuration-option), [test sürücüsü kurulumu](https://learn.microsoft.com/en-us/windows-hardware/drivers/install/how-to-install-test-signed-driver-for-setup-and-boot).

Mevcut imzalı ImDisk'i sırf pagefile destekliyor diye VRAM backend sayamayız: maintainer'ın belgelediği pagefile yolu sistem RAM'inden AWE tahsisidir; kullanıcı modu/CUDA VRAM ve SSD-authoritative cache yolumuz değildir. [ImDisk maintainer açıklaması](https://github.com/LTRData/ImDisk/issues/16).

GUI'deki **Yerel bağlantıyı denetle** kendi Rust broker'ını normal yetkiyle aynı kullanıcı oturumunda başlatır; bağlantı/durum cevabından sonra kapatır. Bu işlem UAC, CUDA tahsisi, sürücü kurulumu veya offload başlatmaz. Windows kontrol düzlemi ve kalan sınırlar [broker belgesinde](INTERACTIVE-BROKER.md).

2026-11-05 gerçek yerel kontrol bağlantısı testi: driver sayısı 0, CI seçenekleri 62465 (`TestSigning=false`), WDDM özellikli helper flag değeri 2. Başlat/durum/durdur, 16 eşzamanlı sorgu, bağlantı kopması ve çocuk çıkışı geçti. Yanlış nonce/PID, tekrar edilen komut ve uzun mesaj gerçek Rust süreçlerinde reddedildi. Ham kayıt: `artifacts/measurements/local-controller-20261105.log`. Test GPU havuzu ayırmadı; sistem RAM offload veya frametime kanıtı değildir.
