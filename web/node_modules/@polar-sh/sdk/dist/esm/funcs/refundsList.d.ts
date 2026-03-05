import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { RefundsListRequest, RefundsListResponse } from "../models/operations/refundslist.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
import { PageIterator } from "../types/operations.js";
/**
 * List Refunds
 *
 * @remarks
 * List products.
 *
 * **Scopes**: `refunds:read` `refunds:write`
 */
export declare function refundsList(client: PolarCore, request: RefundsListRequest, options?: RequestOptions): APIPromise<PageIterator<Result<RefundsListResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>, {
    page: number;
}>>;
//# sourceMappingURL=refundsList.d.ts.map