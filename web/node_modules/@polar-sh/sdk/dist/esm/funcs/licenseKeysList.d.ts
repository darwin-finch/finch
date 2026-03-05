import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { Unauthorized } from "../models/errors/unauthorized.js";
import { LicenseKeysListRequest, LicenseKeysListResponse } from "../models/operations/licensekeyslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List License Keys
 *
 * @remarks
 * Get license keys connected to the given organization & filters.
 *
 * **Scopes**: `license_keys:read` `license_keys:write`
 */
export declare function licenseKeysList(client: PolarCore, request: LicenseKeysListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<LicenseKeysListResponse, Unauthorized | ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=licenseKeysList.d.ts.map