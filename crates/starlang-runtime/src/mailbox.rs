//! Process mailbox for message delivery.
//!
//! Each process has a mailbox that receives messages from other processes.
//! The mailbox uses an unbounded MPSC channel for message delivery.

use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::timeout;

/// A message that can be delivered to a process mailbox.
///
/// Messages are stored as raw bytes to support heterogeneous message types.
#[derive(Debug, Clone)]
pub struct Envelope {
    /// The raw message bytes.
    pub data: Vec<u8>,
}

impl Envelope {
    /// Create a new envelope with the given data.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }
}

/// The receiving end of a process mailbox.
///
/// This is held by the process and used to receive messages.
pub struct Mailbox {
    rx: mpsc::UnboundedReceiver<Envelope>,
}

impl Mailbox {
    /// Creates a new mailbox, returning the mailbox and its sender.
    pub fn new() -> (Self, MailboxSender) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { rx }, MailboxSender { tx })
    }

    /// Receives the next message, blocking until one is available.
    ///
    /// Returns `None` if all senders have been dropped.
    pub async fn recv(&mut self) -> Option<Envelope> {
        self.rx.recv().await
    }

    /// Receives the next message with a timeout.
    ///
    /// Returns `Ok(Some(envelope))` if a message was received,
    /// `Ok(None)` if all senders were dropped,
    /// or `Err(())` if the timeout elapsed.
    pub async fn recv_timeout(&mut self, duration: Duration) -> Result<Option<Envelope>, ()> {
        match timeout(duration, self.rx.recv()).await {
            Ok(msg) => Ok(msg),
            Err(_) => Err(()),
        }
    }

    /// Tries to receive a message without blocking.
    ///
    /// Returns `Ok(envelope)` if a message was available,
    /// `Err(TryRecvError::Empty)` if no message was available,
    /// or `Err(TryRecvError::Disconnected)` if all senders were dropped.
    pub fn try_recv(&mut self) -> Result<Envelope, mpsc::error::TryRecvError> {
        self.rx.try_recv()
    }

    /// Closes the mailbox, preventing any further messages from being sent.
    pub fn close(&mut self) {
        self.rx.close()
    }
}

/// The sending end of a process mailbox.
///
/// This can be cloned and shared between processes to send messages
/// to the mailbox owner.
#[derive(Clone)]
pub struct MailboxSender {
    tx: mpsc::UnboundedSender<Envelope>,
}

impl MailboxSender {
    /// Sends a message to the mailbox.
    ///
    /// Returns `Ok(())` if the message was sent successfully,
    /// or `Err(envelope)` if the mailbox was closed.
    pub fn send(&self, envelope: Envelope) -> Result<(), Envelope> {
        self.tx.send(envelope).map_err(|e| e.0)
    }

    /// Returns `true` if the mailbox is closed.
    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mailbox_send_recv() {
        let (mut mailbox, sender) = Mailbox::new();

        sender.send(Envelope::new(vec![1, 2, 3])).unwrap();
        sender.send(Envelope::new(vec![4, 5, 6])).unwrap();

        let msg1 = mailbox.recv().await.unwrap();
        assert_eq!(msg1.data, vec![1, 2, 3]);

        let msg2 = mailbox.recv().await.unwrap();
        assert_eq!(msg2.data, vec![4, 5, 6]);
    }

    #[tokio::test]
    async fn test_mailbox_try_recv() {
        let (mut mailbox, sender) = Mailbox::new();

        // No message yet
        assert!(mailbox.try_recv().is_err());

        sender.send(Envelope::new(vec![1, 2, 3])).unwrap();

        // Now there's a message
        let msg = mailbox.try_recv().unwrap();
        assert_eq!(msg.data, vec![1, 2, 3]);

        // No more messages
        assert!(mailbox.try_recv().is_err());
    }

    #[tokio::test]
    async fn test_mailbox_timeout() {
        let (mut mailbox, _sender) = Mailbox::new();

        // Should timeout quickly
        let result = mailbox.recv_timeout(Duration::from_millis(10)).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_mailbox_close() {
        let (mut mailbox, sender) = Mailbox::new();

        // Send a message
        sender.send(Envelope::new(vec![1, 2, 3])).unwrap();

        // Close the mailbox
        mailbox.close();

        // Can still receive pending messages
        let msg = mailbox.recv().await.unwrap();
        assert_eq!(msg.data, vec![1, 2, 3]);

        // Further sends should fail
        assert!(sender.send(Envelope::new(vec![4, 5, 6])).is_err());
    }

    #[tokio::test]
    async fn test_sender_is_closed() {
        let (mut mailbox, sender) = Mailbox::new();

        assert!(!sender.is_closed());

        mailbox.close();

        assert!(sender.is_closed());
    }

    #[tokio::test]
    async fn test_multiple_senders() {
        let (mut mailbox, sender1) = Mailbox::new();
        let sender2 = sender1.clone();

        sender1.send(Envelope::new(vec![1])).unwrap();
        sender2.send(Envelope::new(vec![2])).unwrap();

        let msg1 = mailbox.recv().await.unwrap();
        let msg2 = mailbox.recv().await.unwrap();

        // Order is preserved per sender but interleaving depends on timing
        assert!(msg1.data == vec![1] || msg1.data == vec![2]);
        assert!(msg2.data == vec![1] || msg2.data == vec![2]);
        assert_ne!(msg1.data, msg2.data);
    }
}
