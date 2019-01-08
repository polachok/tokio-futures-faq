//! Пример с асинхронными параллельно работающими чтением/записью в сокет.

use futures::Future;
use std::net::SocketAddr;
use tokio::net::TcpListener;

mod server {
    use futures::{Future, Stream};
    use tokio::{
        io::{copy, AsyncRead},
        net::TcpListener,
    };

    /// Простенький ехо-сервер.
    pub fn run(listener: TcpListener) -> impl Future<Item = (), Error = ()> {
        listener
            .incoming()
            .map_err(|err| eprintln!("[server] I/O error while accepting connections: {}", err))
            // `for_each` принимает замыкание, которое возвращает фьючу с `Item = ()` и такой же
            // ошибкой, как и поток, и именно поэтому выше мы мапим ошибку `io::Error` на `()`,
            // чтобы мы могли вернуть фьючу с `Error = ()`. Точнее говоря, ожидается не `Future`,
            // а `IntoFuture`, но это детали :) Ну и именно это и возвращает `tokio::spawn`.
            .for_each(|connection| {
                // Метод `AsyncRead::split()` разделяет поток на две части (при условии, что поток
                // имплементит ещё и `AsyncWrite`).
                let (reader, writer) = connection.split();
                // Просто копируем (асинхронно) всё обратно.
                let echoing = copy(reader, writer)
                    .map(|(bytes_copied, _reader, _writer)| {
                        println!("[server] Copied {} bytes", bytes_copied);
                    })
                    .map_err(|err| {
                        eprintln!(
                            "[server] I/O error while echoing data back to the client: {}",
                            err
                        )
                    });
                // Запускаем фьючу в отдельной таске, то есть на каждое входящее соединение мы создаём
                // отдельную таску. Если этого не делать, то следующее соединение будет обработано
                // только тогда, когда полностью обработано (и, вероятно, закрыто) текущее.
                tokio::spawn(echoing)
            })
    }
}

mod client {
    use futures::{
        future::{loop_fn, Loop},
        stream::unfold,
        sync::oneshot::{channel, Receiver, Sender},
        Future, Stream,
    };
    use rand::{rngs::SmallRng, Rng, SeedableRng};
    use std::{
        net::SocketAddr,
        time::{Duration, Instant},
    };
    use tokio::{
        io::{read, write_all, AsyncRead, AsyncWrite},
        net::TcpStream,
        timer::Delay,
    };

    /// Считываем всё, что сервер имеет нам сказать, либо до момента, когда соединение закрывается
    /// (`bytes_read` == 0), либо когда получаем сигнал к завершению.
    ///
    /// Тут важно отметить, что если клиент закроет соединение до того, как сервер закончит
    /// отправку данных (а такое более чем вероятно, особенно на локалхосте), то при попытке
    /// отправить следующую порцию, он получит ошибку "Connection reset by peer", так что не
    /// удивляйтесь, ежели увидите эту ошибку в самом конце во время запуска примера.
    fn read_all<T: AsyncRead>(
        input: T,
        when_to_end: Receiver<()>,
    ) -> impl Future<Item = (), Error = ()> {
        // В принципе, можно было бы обойтись и без того, чтобы аллоцировать заранее буфер и потом
        // каждый раз гонять его туда-сюда, но это очень полезная техника, позволяющая избежать
        // лишних аллокаций при каждом чтении.
        let buf = vec![0; 200];
        // См. `futures::future::loop_fn`.
        let reading = loop_fn((input, buf), |(input, buf)| {
            // Функция `tokio::io::read` считывает из потока столько данных, сколько получится
            // за один раз, но при этом не больше, чем размер буфера. Впрочем, это очевидно, т.к.
            // она принимает буфер в виде `AsMut<[u8]>` и, соответственно, не имеет возможности
            // как-то повлиять на размер оного.
            read(input, buf)
                .map(move |(input, buf, bytes_read)| {
                    println!("[client] Received {} bytes", bytes_read);
                    if bytes_read == 0 {
                        Loop::Break(bytes_read)
                    } else {
                        Loop::Continue((input, buf))
                    }
                })
                .map_err(move |err| eprintln!("[client] I/O error while reading data: {}", err))
        })
        .map(|bytes| println!("[client] Read {} bytes in total", bytes));
        reading
            .select2(when_to_end.map(|_| println!("[client] Stop reading")))
            .map(|_| ())
            .map_err(|_| ())
    }

    /// Генерирует бесконечный поток, который резолвится через случайные промежутки времени
    /// (от 0 до 30 мсек).
    fn random_instants() -> impl Stream<Item = (), Error = ()> {
        let rng = SmallRng::seed_from_u64(0);
        // См. `futures::stream::unfold`.
        unfold(rng, |mut rng| {
            let duration = Duration::from_micros(rng.gen_range(0, 30));
            let when = Instant::now() + duration;
            // Мы указываем `((), rng)`, потому что `unfold` ожидает, что возвращаемое фьючей
            // значение состоит из того, что будет возвращать генерируемый поток и части,
            // которая будет передана этому замыканию на следующей итерации.
            Some(Delay::new(when).map(move |()| ((), rng)))
        })
        .map_err(|_| ())
    }

    /// Отсылаем вектор случайной длины через случайные промежутки времени.
    fn write_randomly<T: AsyncWrite>(
        output: T,
        sender: Sender<()>,
    ) -> impl Future<Item = (), Error = ()> {
        let rng = SmallRng::seed_from_u64(0);
        let buf = Vec::new();
        random_instants()
            // Достаточно 10 раз.
            .take(10)
            // См `futures::stream::fold`.
            .fold(
                (rng, buf, output),
                move |(mut rng, mut buf, output), _instant| {
                    buf.resize(rng.gen_range(1, 400), 0);
                    write_all(output, buf)
                        .map_err(|err| eprintln!("[client] I/O error while sending data: {}", err))
                        .map(|(output, buf)| {
                            println!("[client] Sent {} bytes", buf.len());
                            (rng, buf, output)
                        })
                },
            )
            .map(|(_rng, _buf, _output)| {})
            .then(move |_| {
                // По завершении отправляем по каналу сообщение о том, что пора сворачиваться.
                sender.send(()).map(|_| ()).map_err(|_| ())
            })
    }

    pub fn run(addr: &SocketAddr) -> impl Future<Item = (), Error = ()> {
        TcpStream::connect(addr)
            .map_err(|err| eprintln!("[client] Can't connect: {}", err))
            .and_then(|socket| {
                // Разбиваем сокет на принимающую (`AsyncRead`) и передающую (`AsyncWrite`) части.
                let (reader, writer) = socket.split();
                // Канал используем для того, чтобы завершить чтение данных от сервера после того,
                // как отправим ему всё, что собирались.
                let (sender, receiver) = channel();
                // Начинаем читать данные от сервера в отдельной таске ...
                tokio::spawn(read_all(reader, receiver));
                // ... а читать будем в текущей. Таким образом эта таска завершится сразу после
                // того, как мы закончим с отправкой данных.
                write_randomly(writer, sender)
            })
    }
}

fn main() {
    // Указываем порт 0, чтобы операционная система сама назначила свободный порт.
    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = TcpListener::bind(&addr).unwrap();
    let addr = listener.local_addr().unwrap();
    // Теперь порт уже должен быть ненулевым.
    assert_ne!(0, addr.port());

    let srv = server::run(listener);
    let client = client::run(&addr);
    tokio::run(
        // Эта комбинированная фьюча завершится либо когда завершится `srv`, либо `client`.
        srv.select(client)
            .map(|((), _next_ready)| ())
            .map_err(|((), _next_ready)| ()),
    );
}
