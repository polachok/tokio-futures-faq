//! Пример работы с кодеком, имеющим внутреннее состояние.

use futures::Future;
use std::net::SocketAddr;
use tokio::net::{TcpListener, TcpStream};

/// В этом модуле будет имплементация протокола (на самом деле кодека).
mod proto {
    use bytes::{BufMut, BytesMut};
    use tokio::codec::{Decoder, Encoder};
    use tokio::io::Error;

    /// Кодек с состоянием: будем считать, сколько сообщений было получено/отправлено.
    pub struct StatefulCodec {
        /// Счётчик отправленных сообщений.
        pub sent_counter: usize,
        /// Счётчик полученных сообщений.
        pub received_counter: usize,
    }

    impl StatefulCodec {
        /// Инициализируем наш кодек.
        pub fn new() -> Self {
            StatefulCodec {
                sent_counter: 0,
                received_counter: 0,
            }
        }
    }

    impl Decoder for StatefulCodec {
        type Item = BytesMut;
        type Error = Error;

        fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
            if src.len() < 10 {
                // Если к чтению доступно меньше 10ти байт, то сообщаем, что нужно подождать ещё.
                Ok(None)
            } else {
                // Вытаскиваем первые 10 байт из `src`.
                let data = src.split_to(10);
                self.received_counter += 1;
                // Всё, сообщение прочитано, отдаём его наружу.
                Ok(Some(data))
            }
        }
    }

    impl Encoder for StatefulCodec {
        type Item = BytesMut;
        type Error = Error;

        fn encode(&mut self, mut item: Self::Item, dst: &mut BytesMut) -> Result<(), Self::Error> {
            // Т.к. наш протокол подразумевает обмен 10-байтовыми сообщениями, то для простоты
            // не будем заморачиваться с обработкой ошибок, а просто либо обрежем входящий массив,
            // либо расширим его, заполнив нулями.
            item.resize(10usize, 0u8);
            // Дописываем массив в `dst`.
            dst.put(item);
            self.sent_counter += 1;
            // Готово! Теперь обработчик кодека (`Framed`) увидит, что в `dst` появились новые
            // данные и отправит их в сокет.
            Ok(())
        }
    }
}

mod server {
    use super::proto::StatefulCodec;
    use futures::{Future, Sink, Stream};
    use tokio::codec::Framed;
    use tokio::net::TcpListener;

    /// Простенький эхо-сервер.
    pub fn echo(listener: TcpListener) -> impl Future<Item = (), Error = ()> {
        listener
            .incoming()
            .for_each(|connection| {
                // `connection` -- это `TcpStream`, он имплементит как `AsyncRead`, так и
                // `AsyncWrite`. Это именно то, что нужно для использования `Framed`.
                let (writer, reader) = Framed::new(connection, StatefulCodec::new()).split();
                // Просто отправляем клиенту обратно всё то, что он отправил нам.
                let processing = writer
                    // `Sink::send_all` отправляет *весь* стрим целиком в сокет. Возвращает фьючу,
                    // которая резолвится, либо когда все данные будут отправлены.
                    .send_all(reader.inspect(|_| println!("[server] Got a message from client")))
                    // Если мы отправили "всё", это значит, что поток входящих сообщений,
                    // завершился, а такое бывает, когда клиент отсоединился.
                    .map(|_| println!("[server] Client disconnected"))
                    .map_err(|err| {
                        eprintln!("[server] I/O error while interracting with client: {}", err)
                    });
                // Запускаем обработку клиента (соединения) в отдельной таске.
                tokio::spawn(processing);
                Ok(())
            })
            .map_err(|err| eprintln!("[server] I/O error while processing connections: {}", err))
    }
}

mod client {
    use super::proto::StatefulCodec;
    use bytes::BytesMut;
    use futures::{
        future::{loop_fn, ok, Either, Loop},
        Future, Sink, Stream,
    };
    use tokio::codec::Framed;
    use tokio::io::Error;
    use tokio::net::TcpStream;

    /// Функция-помощник, отправляет сообщение в канал и сразу после этого считывает сообщение
    /// оттуда же.
    fn send_and_receive<T>(connection: T) -> impl Future<Item = Loop<(), T>, Error = Error>
    where
        T: Sink<SinkItem = BytesMut, SinkError = Error>,
        T: Stream<Item = BytesMut, Error = Error>,
    {
        connection
            .send(vec![0, 1, 2, 3].into())
            .and_then(|connection| {
                connection
                    // `Stream::into_future` используется, чтобы получить первое доступное значение
                    // из стрима.
                    .into_future()
                    .map(|(response, connection)| {
                        if response.is_none() {
                            // Если `response` -- `None`, то это значит, что соединение закрылось.
                            Loop::Break(())
                        } else {
                            println!("[client] Received a response from server");
                            // Сам ответ сервера мы игнорируем, да.
                            Loop::Continue(connection)
                        }
                    })
                    // В случае ошибки мы не проводим никаких восстановительных работ, а просто
                    // завершаемся, прокидывая ошибку дальше.
                    .map_err(|(e, _connection)| e)
            })
    }

    pub fn run(connection: TcpStream) -> impl Future<Item = (), Error = ()> {
        let connection = Framed::new(connection, StatefulCodec::new());
        loop_fn(connection, |connection| {
            if connection.codec().sent_counter >= 10 {
                println!("[client] Already sent 10 messages, terminating");
                // Нам нужен `Either`, потому что `ok(..)` и `send_and_receive(..)` возвращают
                // разные типы (хоть они и оба имплементят один и тот же `Future`), но обе ветки
                // if'а должны возвращать один и тот же тип. Вместо `Either` можно было бы
                // использовать `Box`.
                Either::A(ok(Loop::Break(())))
            } else {
                Either::B(send_and_receive(connection))
            }
        })
        .map_err(|err| eprintln!("[client] I/O error: {}", err))
    }
}

fn main() {
    // Указываем порт 0, чтобы операционная система сама назначила свободный порт.
    let addr: SocketAddr = ([127, 0, 0, 1], 0).into();
    let listener = TcpListener::bind(&addr).unwrap();
    let addr = listener.local_addr().unwrap();
    // Теперь порт уже должен быть ненулевым.
    assert_ne!(0, addr.port());

    let srv = server::echo(listener);
    let client = TcpStream::connect(&addr)
        .map_err(|err| eprintln!("[client] Can't connect: {}", err))
        .and_then(client::run);

    tokio::run(
        // Метод `Future::select` из двух фьюч создаёт одну, которая завершается, когда завершается
        // одна из них.
        srv.select(client)
            .map(|((), _select_next_future)| ())
            .map_err(|((), _select_next_future)| ()),
    );
}
