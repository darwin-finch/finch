import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { NotPermitted } from "../models/errors/notpermitted.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { FilesUploadedRequest, FilesUploadedResponseFilesUploaded } from "../models/operations/filesuploaded.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Complete File Upload
 *
 * @remarks
 * Complete a file upload.
 *
 * **Scopes**: `files:write`
 */
export declare function filesUploaded(client: PolarCore, request: FilesUploadedRequest, options?: RequestOptions): APIPromise<Result<FilesUploadedResponseFilesUploaded, NotPermitted | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=filesUploaded.d.ts.map