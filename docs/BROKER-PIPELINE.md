# Okumayı dolum işinin arkasında bekletmemek

Tek bir bekleyen GET, 1 MiB'lik paging yazmasının 16 adet 64 KiB dilimini alamaz: ilk dilim kullanıcı sürecine geçtiğinde diğer dolumlar hazır GET bulamayabilir. Yeni broker aynı kontrol handle'ında 16 bağımsız GET'i önceden bekletir. Her slotun ayrı, sabit adresli buffer'ı, OVERLAPPED'i ve manual-reset event'i vardır.

Tamamlanmış bir paket, 64 KiB kopyalamadan yedek buffer ile yer değiştirir; boşalan slot GPU işi başlamadan tekrar gönderilir. Hazır READ paketleri, sıra numarası küçük olsa bile FILL paketlerinin önüne alınır. GPU'da başlamış bir kopya kesilmez; sonraki seçimde okumaya öncelik verilir. Aralıklar yine yalnız tam ve güncel hit olarak cevaplanır.

Bu pencere tüm yazma dilimlerinin VRAM'e ulaşacağını garanti etmez. Okumalar, queue limiti veya GPU baskısı dolum kapasitesini tüketebilir; eksik cache verisi daima SSD yoluna bırakılır. Buffer penceresinin kullanıcı ve kernel kopyaları yaklaşık 2 MiB ek alan gerektirir; net RAM hesabından bu maliyet de çıkarılmalıdır.

## Son tarih ve kapanış

Data protokolü v3, READ header'ında kernel'in yakaladığı interrupt-time deadline'ını taşır. Aynı değer pending request state'e yazılır. Header 192 bayt, maksimum frame 65.728 bayttır. Query ABI v4/128 bayt protokol v3'ü bildirir; eski paketler reddedilir.

Broker, GPU işi için en az 250 µs kalmamışsa lookup yapmadan MISS gönderir. GPU'dan hit geldikten sonra süre tekrar kontrol edilir; geç hit MISS'e çevrilir. `QueryInterruptTimePrecise` ile kernel `KeQueryInterruptTimePrecise` belgeleri boot-relative 100 ns interrupt-time sayacını tanımlar. Aynı domain kullanımı bu tanımlara dayanan bir çıkarımdır; güncel canlı kernel doğrulaması henüz yapılmadı. Kernel'in bağımsız timer'ı ve son kontrolü her durumda asıl kapıdır; kullanıcı modu kontrolü OS tamamlanma üst sınırı garantisi değildir. [Win32 saat](https://learn.microsoft.com/en-us/windows/win32/api/realtimeapiset/nf-realtimeapiset-queryinterrupttimeprecise), [kernel saat](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/wdm/nf-wdm-kequeryinterrupttimeprecise).

Kapanışta önce cache devre dışı bırakılır, sonra tüm pending GET'ler için CancelIoEx istenir ve hepsinin tamamlandığı GetOverlappedResult ile doğrulanır. İptal talebi buffer'ın serbest bırakılması için yeterli değildir. File handle, event ve buffer ömürleri bu beklemeden sonra biter. Aynı bağlantıdaki kısa kontrol çağrıları ayrı bir mutex ile ortak event üzerinde serileştirilir; inline başarı da tamamlanan byte sayısı için GetOverlappedResult kullanır.

## Yerel Windows kanıtı

`svram io-smoke`, kendi geçici named pipe'ı ve boş test dosyası üzerinde gerçek Windows API çağrıları çalıştırır. Son ham çıktı `artifacts/measurements/local-pipeline-ranges-20261104.log` içindedir.

- 4–64 KiB paketler: hazır okumalar dolumlardan önce işlendi; tam payload ve sıra numarası kontrol edildi.
- 16 pending okuma iptal edildi ve tamamlanmaları beklendi; açıkça shutdown çağrısı yapılmadan pool drop edildiğinde de eski istekler yeni okumayı çalmadı.
- 32 iptal/tamamlanma yarışı her başarılı payload'ı doğruladı; yarış sayıları scheduler'a göre değişir.
- Büyük/eksik paket ERROR_MORE_DATA ile reddedildi; geçerli header prefix'i başarı sayılmadı.
- Dört thread'in aynı bağlantıdaki 256 FSCTL_GET_COMPRESSION çağrısı aynı doğru byte sayısı ve veriyle tamamlandı. Yalnız yeni oluşturulan boş dosyanın salt okunur durumu sorgulandı ve dosya silindi.

Bu testler KMDF kontrol cihazını, pagefile I/O'sunu veya oyun kazancını doğrulamaz. Yeni sürücü yerel kernel'e yüklenmedi ve yeni sanal makine başlatılmadı. Gerçek CUDA range kanıtı ayrıca [range cache belgesindedir](RANGE-CACHE.md).
