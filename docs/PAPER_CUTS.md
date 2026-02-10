# Paper Cuts

- When pressing Esc console redraw of prompt flickers
- Questions about `virtio::disk_op`:
    - Is it possible to extract the locking and pass the lock to the read or write functinos? I want to save 512 bytes on the stakck without code duplication or unsafe pointers.
    - Need to check whether there are any other unnecessary BLOCK_SIZE copies
- I want to profile avoiding having two `core::fmt::Write` calls for each print. What is the speed cost, what is the memory cost of having a temp buffer. Is the buffer `Sized?`?
- I have not looked at rust docs for a long time. Need to have a session to get this up to decent spec
