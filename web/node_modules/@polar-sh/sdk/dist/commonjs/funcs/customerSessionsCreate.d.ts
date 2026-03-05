import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { CustomerSession } from "../models/components/customersession.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { CustomerSessionsCreateCustomerSessionCreate } from "../models/operations/customersessionscreate.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Create Customer Session
 *
 * @remarks
 * Create a customer session.
 *
 * **Scopes**: `customer_sessions:write`
 */
export declare function customerSessionsCreate(client: PolarCore, request: CustomerSessionsCreateCustomerSessionCreate, options?: RequestOptions): APIPromise<Result<CustomerSession, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=customerSessionsCreate.d.ts.map