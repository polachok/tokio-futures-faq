//! Пример получения данных из `stdio`.

use futures::{sync::mpsc, Future, Stream};
use std::{io, thread, time::Duration};
use tokio::timer::Interval;

/// Эта функция возвращает объект типа `Stream`, который можно использовать для асинхронного
/// получения данных со стандартного ввода (построчно).
fn input_reader() -> impl Stream<Item = String, Error = ()> {
    let (sender, receiver) = mpsc::unbounded();
    thread::spawn(move || {
        let input = io::stdin();
        let mut buf = String::new();
        loop {
            buf.clear();
            // Вызов `read_line` заблокирует поток, и именно из-за этого мы и запускаем
            // считывание в отдельном потоке.
            if let Err(err) = input.read_line(&mut buf) {
                eprintln!("Encountered an I/O error while reading from stdin: {}", err);
                break;
            }
            if buf.is_empty() {
                // Пустой буфер означает, что стандартный ввод закрыт (EOF).
                // Когда мы выходим из цикла и, соответственно, из замыкания, то единственный
                // доступный отправитель (`sender`) дропается, и, таким образом, получатель
                // (`receiver`) узнает, что данных больше не будет и завершается (как поток).
                break;
            }
            // Метод `UnboundedSender::unbounded_send` не требует, чтобы его вызывали только внутри
            // контекста (т.е. изнутри фьючи или стрима или ещё чего), чем мы и пользуемся.
            if sender.unbounded_send(buf.clone()).is_err() {
                // Единственная ошибка, которую может вернуть `UnboundedSender`, означает, что
                // получатель был дропнут, соответственно, нет смысла продолжать дальше.
                break;
            }
        }
    });
    receiver
}

/// Ожидает либо когда пользователь введёт `exit`, либо когда закроет стандартный ввод (Ctrl+D
/// в линуксе, Cmd+D в макоси, Ctrl+Z и Enter в виндовсе).
fn wait_for_exit() -> impl Future<Item = (), Error = ()> {
    input_reader()
        // Отсеиваем любой ввод, отличающийся от `exit`.
        .filter(|arg| arg.trim_end() == "exit")
        // Метод `Stream::into_future` делает следующую магию: он возвращает объект типа `Future`,
        // который резолвится в элемент, возвращаемый стримом (или `None`, если стрим завершился
        // до того, как что-то вернуть), и, собственно, в сам стрим, то есть
        // `(Option<Stream::Item>, Stream)`.
        .into_future()
        .map(|(message, _rest_of_the_stream)| {
            if message.is_none() {
                println!("`stdin` terminated, exiting");
            } else {
                println!("Received `exit` command, exiting");
            }
            // Замыкание возвращает юнит `()`!
        })
        .map_err(|((), _rest_of_the_stream)| ())
}

/// Эмуляция каких-то асинхронных вычислений.
///
/// В данном случае просто печатаем таймстамп каждые полсекунды.
fn calculation() -> impl Future<Item = (), Error = ()> {
    Interval::new_interval(Duration::from_millis(500))
        .for_each(|timestamp| {
            println!("{:?}", timestamp);
            Ok(())
        })
        .map_err(|err| eprintln!("Encountered a timer error: {}", err))
}

fn main() {
    println!("To stop the execution either type `exit` and hit Enter or send EOF.");
    tokio::run(
        calculation()
            // Метод `Future::select` из двух фьюч создаёт одну, которая завершается, когда
            // завершается одна из них.
            .select(wait_for_exit())
            .map(|((), _select_next_future)| ())
            .map_err(|((), _select_next_future)| ()),
    );
    println!("Done");
}
