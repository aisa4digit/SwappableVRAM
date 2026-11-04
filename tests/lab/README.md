# İzole Windows repro ortamı

Bu harness, kendi QEMU diskleri üzerinde WinPE açar. GPU passthrough, host disk bağlantısı, ağ, görünür pencere, host sertifika kaydı veya host boot ayarı değişikliği yoktur. 1536 MiB RAM ve tek düşük öncelikli vCPU kullanır. WinPE, masaüstü Windows/gerçek GPU/Driver Verifier/oyun performansı doğrulaması yerine geçmez.

## Hazırlık

PowerShell 7, Rust/MSVC ve Windows `tar.exe` gerekir. Betikler kurucuları çalıştırmaz; araçlar `artifacts/toolchain` altında tutulur. Çalışan proje VM'si varken araçlar yeniden açılamaz.

```powershell
pwsh -File scripts/Prepare-VmTools.ps1 -FetchVerifiedTools
pwsh -File scripts/Prepare-WinPeAssets.ps1 -FetchVerifiedAssets
cargo build --release --manifest-path native/Cargo.toml
pwsh -File scripts/Prepare-VmInventory.ps1
```

Son komut yeni bir `artifacts/vm-lab/inventory-...` dizini üretir. Kaynak WIM'in özel kopyasına iki CMD dosyası eklenir ve WIM doğrulanır. QEMU'nun yalnız okunur FAT görünümü, bağımsız `boot.raw` dosyasına dönüştürülür. VM bu dizine doğrudan bağlı değildir; yalnız `boot.raw`, `target.raw` ve özel firmware-vars dosyasını açar.

## Çalıştırma

```powershell
pwsh -File scripts/Start-VmInventory.ps1 -RunDirectory <üretilen-mutlak-dizin>
pwsh -File scripts/Start-VmInventory.ps1 -RunDirectory <aynı-mutlak-dizin>
```

İlk açılış, yalnız konuk boot diskindeki BCD kopyalarında test imzasını ayarlar ve kapanır. İkinci **soğuk açılış**, runtime code-integrity durumunu ve PnP disklerini kaydeder. QEMU/WHPX warm reset bu harness için doğrulanmadığından iki ayrı süreç kullanılır. Konuk `GUEST.LOG` handle'ını kapattıktan sonra kapanır; ana makine logu yalnız QEMU çıktıktan sonra disk imajından okur. Süre sınırı aşılırsa yalnız ilgili VM PID'si durdurulur. Aynı fixture canlıyken ikinci kopyası başlatılamaz.

`-ObserveShutdown`, hata incelemesinde konuk power-off sonrasında QEMU'yu süre sınırına kadar tutar. `Invoke-LabQmp.ps1` yalnız canlı olduğu doğrulanan aynı fixture üzerinde durum, ekran görüntüsü veya quit komutuna izin verir.

## Geçmiş kernel kanıtı ve güncel ABI

Kaynak artık query ABI v4/128 bayt ve range data protocol v3 kullanır. Belgelenmiş geçmiş WinPE başarıları 4 KiB prototipinin ABI v2/v3 sonuçlarıdır; güncel 4–64 KiB range kodunun kernel doğrulaması değildir. Yeni paket ile CLI aynı ABI'de olmalıdır. Çalışma yerel bilgisayara yönlendirildiğinden yeni VM testi başlatılmıyor.

## Bypass sürücüsü testi

Önce envanterde tek `SCSI\DiskSVRAMLABPAGINGDISK______` hedefi bulunduğunu doğrulayın. Paket yalnız bu HWID'ye eşleşmelidir; INF HWID eşleşmesi tek instance'a bağlama garantisi değildir.

```powershell
pwsh -File scripts/Prepare-LabPackage.ps1 -HardwareId 'SCSI\DiskSVRAMLABPAGINGDISK______' -Catalog
pwsh -File scripts/Sign-LabPackage.ps1 -PackageDirectory artifacts/driver-package-bypass -TestOnly
pwsh -File scripts/Prepare-VmInventory.ps1 -DriverPackageDirectory artifacts/driver-package-bypass
```

Hazırlık paketin manifest/hash'lerini kontrol eder ve yalnız test imzalı **bypass** paketini kabul eder. Yeni fixture yine iki defa açılır. İkinci oturumda geçici sertifika yalnız WinPE LocalSystem guard'ından sonra konuğun Root/TrustedPublisher deposuna eklenir; `pnputil` dar HWID için INF'yi yükler ve test cihazını yeniden başlatır. Başarı kanıtı yalnız INF kurulumu değildir: `SVRAM_BYPASS_ATTACH_VERIFIED` için tek özel arayüz, doğru PnP instance ve canlı query ABI v4 / mode 0 yanıtı gerekir.

Sonraki storage adımı, yalnız WinPE/LocalSystem/runtime test-sign guard'larından sonra tek SVRAMLAB SCSI PDO'yu ve tam 1 GiB boyutunu doğrular. Boş R: harfi ister, yalnız bu disposable diski NTFS olarak biçimlendirir ve R: disk extent'ini aynı fiziksel disk numarasına yeniden eşler. 256 adet doğrudan 4 KiB yazma/okuma bütünlüğünden sonra konukta 256 MiB pagefile ister. `SVRAM_PAGEFILE_PATH_VERIFIED` için sürücünün paging-path bildirimi sayacı da artmalıdır. Bu marker bile paging-read cache hit'i veya net RAM kazancı anlamına gelmez.

Deneysel cache paketi yalnız açık `-ExperimentalScenario miss|late|exit` seçimiyle kabul edilir. Konuk marker'ı aynı senaryoyu kendi RAM-fixture broker'ına iletir; gerçek GPU kullanmaz. `late` okumada 20 ms gecikir, `miss` bütün okumaları diske bırakır, `exit` ilk kernel okumasında süreçten 87 koduyla çıkar. Dosya bütünlüğü, paging bildirimi, ilgili sıfırdan büyük hata sayaçları ve kapanışta sıfır cache lease ayrıca aranır. Host doğrulayıcısı yalnız son açılış oturumunun marker'larını kabul eder; önceki stage-1 başarısı sonraki hatayı gizleyemez. Hiçbir WinPE repro sonucu, gerçek masaüstü Windows üzerinde oyun performansı artışı iddia etmez.
