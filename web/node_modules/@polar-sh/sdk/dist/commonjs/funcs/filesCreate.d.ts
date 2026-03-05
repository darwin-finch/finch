import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { FileCreate } from "../models/components/filecreate.js";
import { FileUpload } from "../models/components/fileupload.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create File
 *
 * @remarks
 * Create a file.
 *
 * **Scopes**: `files:write`
 */
export declare function filesCreate(client: PolarCore, request: FileCreate, options?: RequestOptions): APIPromise<Result<FileUpload, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=filesCreate.d.ts.map